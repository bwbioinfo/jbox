//! Read-only PCI observations, not a passthrough readiness check.
//!
//! Never open device nodes or write sysfs. Keep roots injectable for offline tests.
use std::fs;
use std::io;
use std::path::Path;

pub fn diagnostics() -> String {
    inspect(Path::new("/sys/bus/pci/devices"), Path::new("/dev/vfio"))
}

fn problem(error: &io::Error) -> String {
    match error.kind() {
        io::ErrorKind::NotFound => "missing".into(),
        io::ErrorKind::PermissionDenied => "permission denied".into(),
        _ => format!("unavailable ({error})"),
    }
}

fn attribute(path: &Path) -> String {
    match fs::read_to_string(path) {
        Ok(value) => value.trim().to_owned(),
        Err(error) => problem(&error),
    }
}

fn link_name(path: &Path) -> Result<String, io::Error> {
    let target = fs::read_link(path)?;
    target
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "link has no final component"))
}

fn presence(path: &Path) -> String {
    match fs::metadata(path) {
        Ok(_) => "present (existence only, usability not checked)".into(),
        Err(error) => problem(&error),
    }
}

fn inspect(pci: &Path, vfio: &Path) -> String {
    let mut lines = vec![
        "GPU passthrough: not implemented in Jbox (read-only diagnostics only)".into(),
        "  These observations do not establish isolation or passthrough readiness.".into(),
        format!(
            "  VFIO control {}: {}",
            vfio.join("vfio").display(),
            presence(&vfio.join("vfio"))
        ),
    ];
    let entries = match fs::read_dir(pci) {
        Ok(entries) => entries,
        Err(error) => {
            lines.push(format!("  PCI enumeration: {}", problem(&error)));
            return lines.join("\n");
        }
    };
    let mut devices = Vec::new();
    for entry in entries {
        match entry {
            Ok(entry) => devices.push(entry.path()),
            Err(error) => lines.push(format!("  PCI entry: {}", problem(&error))),
        }
    }
    devices.sort();
    let mut found = 0;
    let mut unknown = 0;
    for device in devices {
        let class = match fs::read_to_string(device.join("class")) {
            Ok(value) => value,
            Err(error) => {
                unknown += 1;
                lines.push(format!("  {} class: {}", device.display(), problem(&error)));
                continue;
            }
        };
        let class = class.trim().trim_start_matches("0x");
        let class = match u32::from_str_radix(class, 16) {
            Ok(class) if class <= 0xffffff => class,
            _ => {
                unknown += 1;
                lines.push(format!("  {} class: invalid PCI class", device.display()));
                continue;
            }
        };
        // PCI base class 03 includes VGA, 3D, and other display controllers.
        if class >> 16 != 0x03 {
            continue;
        }
        found += 1;
        let address = device.file_name().unwrap_or_default().to_string_lossy();
        lines.push(format!(
            "  GPU {address}: class {class:06x}, vendor {}, device {}",
            attribute(&device.join("vendor")),
            attribute(&device.join("device"))
        ));
        lines.push(format!(
            "    driver: {}",
            link_name(&device.join("driver")).unwrap_or_else(|error| problem(&error))
        ));
        match link_name(&device.join("iommu_group")) {
            Ok(group) => {
                lines.push(format!("    IOMMU group: {group}"));
                match fs::read_dir(device.join("iommu_group/devices")) {
                    Ok(entries) => {
                        let mut members = Vec::new();
                        for entry in entries {
                            match entry {
                                Ok(entry) => {
                                    members.push(entry.file_name().to_string_lossy().into_owned())
                                }
                                Err(error) => lines
                                    .push(format!("    IOMMU group member: {}", problem(&error))),
                            }
                        }
                        members.sort();
                        lines.push(format!(
                            "    group members: {}",
                            if members.is_empty() {
                                "none observed".into()
                            } else {
                                members.join(", ")
                            }
                        ));
                    }
                    Err(error) => lines.push(format!("    group members: {}", problem(&error))),
                }
                // A group node may be absent even on hosts using other VFIO APIs.
                lines.push(format!(
                    "    VFIO group {}: {}",
                    vfio.join(&group).display(),
                    presence(&vfio.join(&group))
                ));
            }
            Err(error) => lines.push(format!("    IOMMU group: {}", problem(&error))),
        }
    }
    if found == 0 {
        lines.push(format!(
            "  No display/3D GPU devices observed ({unknown} unreadable or invalid PCI classes)."
        ));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use tempfile::tempdir;

    #[test]
    fn empty_and_missing_sysfs_are_nonfatal() {
        let root = tempdir().unwrap();
        let output = inspect(root.path(), &root.path().join("vfio"));
        assert!(output.contains("No display/3D GPU devices observed"));
        assert!(output.contains("not implemented in Jbox"));
        assert!(output.contains("missing"));
        assert!(
            inspect(&root.path().join("absent"), root.path()).contains("PCI enumeration: missing")
        );
    }

    #[test]
    fn reports_gpu_and_all_group_members_without_readiness_claims() {
        let root = tempdir().unwrap();
        let pci = root.path().join("pci");
        let gpu = pci.join("0000:01:00.0");
        let audio = pci.join("0000:01:00.1");
        let group = root.path().join("iommu_groups/7");
        let vfio = root.path().join("vfio");
        for path in [&gpu, &audio, &group.join("devices"), &vfio] {
            fs::create_dir_all(path).unwrap();
        }
        for (name, value) in [
            ("class", "0x030200\n"),
            ("vendor", "0x10de\n"),
            ("device", "0x28a1\n"),
        ] {
            fs::write(gpu.join(name), value).unwrap();
        }
        fs::write(audio.join("class"), "0x040300").unwrap();
        symlink("/sys/bus/pci/drivers/nvidia", gpu.join("driver")).unwrap();
        symlink(&group, gpu.join("iommu_group")).unwrap();
        symlink(&gpu, group.join("devices/0000:01:00.0")).unwrap();
        symlink(&audio, group.join("devices/0000:01:00.1")).unwrap();
        fs::write(vfio.join("7"), "").unwrap();
        let output = inspect(&pci, &vfio);
        for expected in [
            "vendor 0x10de, device 0x28a1",
            "driver: nvidia",
            "IOMMU group: 7",
            "group members: 0000:01:00.0, 0000:01:00.1",
            "existence only, usability not checked",
            "not implemented in Jbox",
        ] {
            assert!(output.contains(expected), "{output}");
        }
        assert!(!output.contains("GPU 0000:01:00.1:"));
    }

    #[test]
    fn malformed_and_unreadable_attributes_do_not_hide_errors() {
        let root = tempdir().unwrap();
        for (name, class) in [
            ("invalid", Some("garbage")),
            ("missing", None),
            ("gpu", Some("0x030000")),
        ] {
            let device = root.path().join(name);
            fs::create_dir(&device).unwrap();
            if let Some(class) = class {
                fs::write(device.join("class"), class).unwrap();
            }
        }
        let output = inspect(root.path(), root.path());
        assert!(output.contains("invalid PCI class"));
        assert!(output.contains("class: missing"));
        assert!(output.contains("vendor missing, device missing"));
        assert!(output.contains("driver: missing"));
        assert!(output.contains("IOMMU group: missing"));
        // Deterministic even under root, where chmod cannot simulate EACCES.
        assert_eq!(
            problem(&io::Error::from(io::ErrorKind::PermissionDenied)),
            "permission denied"
        );
        fs::write(root.path().join("not-a-directory"), "").unwrap();
        assert!(
            inspect(&root.path().join("not-a-directory"), root.path())
                .contains("PCI enumeration: unavailable")
        );
    }
}
