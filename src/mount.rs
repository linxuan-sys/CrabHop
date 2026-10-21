//! The devices UDisks2 knows about (P17): listed, mounted and unmounted over the system bus with
//! zbus — no subprocess, no `gio`, no shell.
//!
//! Everything the panel shows is a pure model: the bus's own dictionaries are read into [`Raw`],
//! and `device_from` / `devices_from` / the badges and rows are plain functions a test can hand a
//! dictionary to. The D-Bus calls themselves live at the bottom, on the blocking API, meant to be
//! run on a worker thread.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

/// One device as the panel reads it.
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct Device {
	/// The D-Bus object path of the block: what mount and unmount are called on.
	pub id:    String,
	/// What the row reads first: the label, or the device node when there is no label.
	pub label: String,
	/// The block device node (`/dev/nvme0n1p2`).
	pub node:  String,
	pub size:  u64,
	/// The filesystem's short name (`ext4`), empty when the block holds none (a whole disk).
	pub fs:    String,
	/// Where it is mounted, when it is.
	pub mounts: Vec<PathBuf>,
	/// The drive's own properties, when the block names a drive.
	pub ejectable: bool,
	pub removable: bool,
}

impl Device {
	pub(crate) fn mounted(&self) -> bool { !self.mounts.is_empty() }

	/// The badge the row wears (P17's three states): a mounted device that can be ejected says
	/// so, any other mounted one says where, and an unmounted one with a filesystem says it could
	/// be. A block with no filesystem at all is not offered.
	pub(crate) fn badge(&self) -> Option<(Badge, &'static str)> {
		if self.mounted() {
			return Some(if self.ejectable { (Badge::Ejectable, "可弹出") } else { (Badge::Mounted, "已挂载") });
		}
		if self.fs.is_empty() {
			return None;
		}

		Some((Badge::Mountable, "未挂载"))
	}

	/// The row the panel draws: left is the label and node, right is the filesystem, the size and
	/// the badge — the four facts a device row carries.
	pub(crate) fn row(&self) -> (String, String) {
		let mut right = String::new();
		if !self.fs.is_empty() {
			right.push_str(&self.fs);
			right.push_str("  ");
		}
		right.push_str(&crate::list::size(self.size));

		if let Some((badge, words)) = self.badge() {
			right.push_str("  ");
			right.push_str(words);
			if badge == Badge::Mounted
				&& let Some(at) = self.mounts.first()
			{
				right.push(' ');
				right.push_str(&at.display().to_string());
			}
		}

		(format!("{}  {}", self.label, self.node), right)
	}

	/// What the panel sorts by: the mounted ones first (they are where a person goes), then the
	/// mountable ones, then by label.
	pub(crate) fn order(&self) -> (u8, String) {
		let rank = match self.badge() {
			Some((Badge::Mounted | Badge::Ejectable, _)) => 0,
			_ => 1,
		};

		(rank, self.label.to_lowercase())
	}
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Badge {
	Mounted,
	Mountable,
	Ejectable,
}

/// One block as the bus hands it over, before the model decides anything: the keys are UDisks2's
/// own (IdLabel, IdType, Size, Device, PreferredDevice, MountPoints, Ejectable, Removable).
#[derive(Clone, Default, PartialEq, Debug)]
pub(crate) struct Raw {
	pub id:        String,
	pub label:     String,
	pub node:      String,
	pub preferred: String,
	pub fs:        String,
	pub size:      u64,
	pub mounts:    Vec<PathBuf>,
	pub ejectable: bool,
	pub removable: bool,
}

/// One raw block into the model, if it is worth a row: something mounted, or something with a
/// filesystem to mount. A bare disk with neither is not a place to go.
pub(crate) fn device_from(raw: Raw) -> Option<Device> {
	let node = if raw.node.is_empty() { raw.preferred.clone() } else { raw.node.clone() };
	if node.is_empty() && raw.mounts.is_empty() {
		return None;
	}
	if raw.mounts.is_empty() && raw.fs.is_empty() {
		return None;
	}

	let label = if raw.label.is_empty() {
		node.rsplit('/').next().unwrap_or(&node).to_owned()
	} else {
		raw.label.clone()
	};

	Some(Device {
		id:        raw.id,
		label,
		node,
		size:      raw.size,
		fs:        raw.fs,
		mounts:    raw.mounts,
		ejectable: raw.ejectable,
		removable: raw.removable,
	})
}

/// The whole list, in the panel's own order.
pub(crate) fn devices_from(raws: Vec<Raw>) -> Vec<Device> {
	let mut devices: Vec<Device> = raws.into_iter().filter_map(device_from).collect();
	devices.sort_by_key(Device::order);

	devices
}

/// The device a path stands on: the one whose mount point is the path itself or its longest
/// ancestor (a directory on `/home` is on the `/home` device, not on `/`).
pub(crate) fn device_of<'a>(devices: &'a [Device], path: &Path) -> Option<&'a Device> {
	devices
		.iter()
		.filter(|device| device.mounts.iter().any(|at| path.starts_with(at)))
		.max_by_key(|device| device.mounts.iter().map(|at| at.components().count()).max().unwrap_or(0))
}

// ---- the bus itself: blocking calls, meant for a worker thread ----

const SERVICE: &str = "org.freedesktop.UDisks2";

/// Every block UDisks2 manages, read through the object manager.
pub(crate) fn list() -> Result<Vec<Device>, String> {
	let connection = zbus::blocking::Connection::system().map_err(wire)?;
	let manager = zbus::blocking::Proxy::new(
		&connection,
		SERVICE,
		"/org/freedesktop/UDisks2",
		"org.freedesktop.DBus.ObjectManager",
	)
	.map_err(wire)?;
	let objects: HashMap<OwnedObjectPath, HashMap<String, HashMap<String, OwnedValue>>> =
		manager.call("GetManagedObjects", &()).map_err(wire)?;

	let mut raws = Vec::new();
	for (path, interfaces) in objects {
		let Some(block) = interfaces.get("org.freedesktop.UDisks2.Block") else { continue };
		let Some(usage) = text(block, "IdUsage") else { continue };
		// a block that holds no filesystem and manages none in particular is not a destination
		if usage != "filesystem" && text(block, "IdType").is_none() {
			continue;
		}

		let filesystem = interfaces.get("org.freedesktop.UDisks2.Filesystem");
		let drive = interfaces.get("org.freedesktop.UDisks2.Drive");

		raws.push(Raw {
			id:        path.to_string(),
			label:     text(block, "IdLabel").unwrap_or_default(),
			node:      bytes(block, "Device").unwrap_or_default(),
			preferred: bytes(block, "PreferredDevice").unwrap_or_default(),
			fs:        text(block, "IdType").unwrap_or_default(),
			size:      number(block, "Size").unwrap_or(0),
			mounts:    filesystem.map(mounts_of).unwrap_or_default(),
			ejectable: drive.is_some_and(|drive| flag(drive, "Ejectable")),
			removable: drive.is_some_and(|drive| flag(drive, "Removable")),
		});
	}

	Ok(devices_from(raws))
}

/// Mount the device; the answer is where it landed.
pub(crate) fn mount(id: &str) -> Result<PathBuf, String> {
	let connection = zbus::blocking::Connection::system().map_err(wire)?;
	let filesystem = proxy(&connection, id, "org.freedesktop.UDisks2.Filesystem")?;
	let options: HashMap<&str, Value<'_>> = HashMap::new();
	let (at,): (String,) = filesystem.call("Mount", &(options)).map_err(wire)?;

	Ok(PathBuf::from(at))
}

/// Unmount the device — and eject the drive it lives on, when asked and when it has one.
pub(crate) fn unmount(id: &str, force: bool, eject: bool) -> Result<(), String> {
	let connection = zbus::blocking::Connection::system().map_err(wire)?;
	let filesystem = proxy(&connection, id, "org.freedesktop.UDisks2.Filesystem")?;
	let mut options: HashMap<&str, Value<'_>> = HashMap::new();
	if force {
		options.insert("force", true.into());
	}
	filesystem.call::<_, _, ()>("Unmount", &(options)).map_err(wire)?;

	if eject {
		let block = proxy(&connection, id, "org.freedesktop.UDisks2.Block")?;
		let drive: OwnedObjectPath = block.get_property("Drive").map_err(wire)?;
		if !drive.as_str().is_empty() && drive.as_str() != "/" {
			let drive = proxy(&connection, drive.as_str(), "org.freedesktop.UDisks2.Drive")?;
			let options: HashMap<&str, Value<'_>> = HashMap::new();
			// an eject that is refused is not worth undoing the unmount over
			let _ = drive.call::<_, _, ()>("Eject", &(options));
		}
	}

	Ok(())
}

fn proxy<'a>(
	connection: &'a zbus::blocking::Connection,
	path: &'a str,
	interface: &'a str,
) -> Result<zbus::blocking::Proxy<'a>, String> {
	zbus::blocking::Proxy::new(connection, SERVICE, path, interface).map_err(wire)
}

fn wire(error: zbus::Error) -> String { format!("{error}") }

/// A string property, when the bus carries one.
fn text(map: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
	map.get(key).and_then(|value| String::try_from(value.clone()).ok())
}

/// A byte-array property read as text: UDisks2's device nodes are `ay`, not `s`.
fn bytes(map: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
	let bytes = map.get(key).and_then(|value| Vec::<u8>::try_from(value.clone()).ok())?;

	Some(String::from_utf8_lossy(&bytes).into_owned())
}

fn number(map: &HashMap<String, OwnedValue>, key: &str) -> Option<u64> {
	map.get(key).and_then(|value| u64::try_from(value.clone()).ok())
}

fn flag(map: &HashMap<String, OwnedValue>, key: &str) -> bool {
	map.get(key).is_some_and(|value| bool::try_from(value.clone()).unwrap_or(false))
}

/// The mount points of a filesystem interface: `aay`, each a path's own bytes.
fn mounts_of(map: &HashMap<String, OwnedValue>) -> Vec<PathBuf> {
	let Some(value) = map.get("MountPoints") else { return Vec::new() };

	Vec::<Vec<u8>>::try_from(value.clone())
		.unwrap_or_default()
		.into_iter()
		.map(|bytes| PathBuf::from(String::from_utf8_lossy(&bytes).into_owned()))
		.collect()
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::list;

	/// The three badges: a mounted ejectable drive says so, another mounted device says where, and
	/// an unmounted one with a filesystem says it could be mounted. A bare disk is not offered.
	#[test]
	fn the_badges_read_the_states_a_person_cares_about() {
		let mut device = Device {
			id: "/block/nvme0n1p2".to_owned(),
			label: "root".to_owned(),
			node: "/dev/nvme0n1p2".to_owned(),
			size: 50 * 1024 * 1024 * 1024,
			fs: "ext4".to_owned(),
			mounts: vec![ PathBuf::from("/") ],
			ejectable: false,
			removable: false,
		};
		assert_eq!(device.badge(), Some((Badge::Mounted, "已挂载")));

		device.ejectable = true;
		assert_eq!(device.badge(), Some((Badge::Ejectable, "可弹出")), "an ejectable mounted one says so");

		device.ejectable = false;
		device.mounts.clear();
		assert_eq!(device.badge(), Some((Badge::Mountable, "未挂载")), "unmounted but fs-bearing");

		device.fs.clear();
		assert_eq!(device.badge(), None, "nothing to mount and nowhere mounted: not a destination");
	}

	/// A block becomes a device when it is mounted or has a filesystem; a whole disk with neither
	/// is left out, a label falls back to the node's own name, and `ay` fields read as text.
	#[test]
	fn a_raw_block_becomes_a_row_when_there_is_somewhere_to_go() {
		let root = Raw {
			id: "/block/nvme0n1p2".to_owned(),
			label: String::new(),
			node: "/dev/nvme0n1p2".to_owned(),
			fs: "ext4".to_owned(),
			size: 4096,
			mounts: vec![ PathBuf::from("/") ],
			..Default::default()
		};
		let device = device_from(root.clone()).expect("a mounted block");
		assert_eq!(device.label, "nvme0n1p2", "no label: the node names it");
		assert!(device.mounted());

		let bare = Raw { fs: String::new(), mounts: Vec::new(), node: "/dev/nvme0n1".to_owned(), ..Default::default() };
		assert!(device_from(bare).is_none(), "a bare disk is not a destination");

		let mountable = Raw { node: "/dev/sdb1".to_owned(), fs: "vfat".to_owned(), ..Default::default() };
		let device = device_from(mountable).expect("a filesystem is somewhere to go");
		assert_eq!(device.badge(), Some((Badge::Mountable, "未挂载")));
	}

	/// The panel's order: the mounted ones first, then the mountable, each by label — and the row
	/// carries the four facts the design asks for.
	#[test]
	fn the_list_orders_mounted_first_and_the_row_carries_its_facts() {
		let devices = devices_from(vec![
			Raw { id: "/b/usb".to_owned(), label: "UDISK".to_owned(), node: "/dev/sda1".to_owned(), fs: "vfat".to_owned(), size: 32 * 1024 * 1024 * 1024, ..Default::default() },
			Raw { id: "/b/root".to_owned(), label: "root".to_owned(), node: "/dev/nvme0n1p2".to_owned(), fs: "ext4".to_owned(), size: 50 * 1024 * 1024 * 1024, mounts: vec![ PathBuf::from("/") ], ..Default::default() },
			Raw { id: "/b/home".to_owned(), label: "home".to_owned(), node: "/dev/nvme0n1p3".to_owned(), fs: "ext4".to_owned(), size: 903 * 1024 * 1024 * 1024, mounts: vec![ PathBuf::from("/home") ], ejectable: true, ..Default::default() },
		]);

		let labels: Vec<&str> = devices.iter().map(|device| device.label.as_str()).collect();
		assert_eq!(labels, vec![ "home", "root", "UDISK" ], "mounted first (labels in order), then the mountable");

		let (left, right) = devices[0].row();
		assert!(left.contains("home") && left.contains("/dev/nvme0n1p3"), "{left}");
		assert!(right.contains("ext4") && right.contains(&list::size(903 * 1024 * 1024 * 1024)), "{right}");
		assert!(right.contains("可弹出"), "the badge says the state: {right}");

		let (_, right) = devices[2].row();
		assert!(right.contains("未挂载") && !right.contains("已挂载"), "the usb stick is not mounted: {right}");
	}

	/// Which device a path stands on: the longest matching mount point wins, so a directory under
	/// `/home` is on the `/home` device rather than the root one.
	#[test]
	fn a_path_stands_on_its_longest_mount() {
		let devices = devices_from(vec![
			Raw { id: "/b/root".to_owned(), label: "root".to_owned(), node: "/dev/a".to_owned(), fs: "ext4".to_owned(), mounts: vec![ PathBuf::from("/") ], ..Default::default() },
			Raw { id: "/b/home".to_owned(), label: "home".to_owned(), node: "/dev/b".to_owned(), fs: "ext4".to_owned(), mounts: vec![ PathBuf::from("/home") ], ..Default::default() },
		]);

		assert_eq!(device_of(&devices, Path::new("/home/x/dev")).map(|d| d.label.as_str()), Some("home"));
		assert_eq!(device_of(&devices, Path::new("/etc")).map(|d| d.label.as_str()), Some("root"));
		assert_eq!(
			device_of(&devices, Path::new("/media/usb")).map(|d| d.label.as_str()),
			Some("root"),
			"everything the fixtures do not claim lives on the root device"
		);
		assert_eq!(device_of(&[], Path::new("/etc")), None, "and no devices at all is no device");
	}
}