
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

#[derive(Clone, PartialEq, Debug)]
pub(crate) struct Device {
	pub id:    String,
	pub label: String,
	pub node:  String,
	pub size:  u64,
	pub fs:    String,
	pub mounts: Vec<PathBuf>,
	pub ejectable: bool,
	pub removable: bool,
}

impl Device {
	pub(crate) fn mounted(&self) -> bool { !self.mounts.is_empty() }

	pub(crate) fn badge(&self) -> Option<(Badge, &'static str)> {
		if self.mounted() {
			return Some(if self.ejectable { (Badge::Ejectable, "可弹出") } else { (Badge::Mounted, "已挂载") });
		}
		if self.fs.is_empty() {
			return None;
		}

		Some((Badge::Mountable, "未挂载"))
	}

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

pub(crate) fn devices_from(raws: Vec<Raw>) -> Vec<Device> {
	let mut devices: Vec<Device> = raws.into_iter().filter_map(device_from).collect();
	devices.sort_by_key(Device::order);

	devices
}

pub(crate) fn device_of<'a>(devices: &'a [Device], path: &Path) -> Option<&'a Device> {
	devices
		.iter()
		.filter(|device| device.mounts.iter().any(|at| path.starts_with(at)))
		.max_by_key(|device| device.mounts.iter().map(|at| at.components().count()).max().unwrap_or(0))
}

const SERVICE: &str = "org.freedesktop.UDisks2";

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

pub(crate) fn mount(id: &str) -> Result<PathBuf, String> {
	let connection = zbus::blocking::Connection::system().map_err(wire)?;
	let filesystem = proxy(&connection, id, "org.freedesktop.UDisks2.Filesystem")?;
	let options: HashMap<&str, Value<'_>> = HashMap::new();
	let (at,): (String,) = filesystem.call("Mount", &(options)).map_err(wire)?;

	Ok(PathBuf::from(at))
}

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

fn text(map: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
	map.get(key).and_then(|value| String::try_from(value.clone()).ok())
}

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
