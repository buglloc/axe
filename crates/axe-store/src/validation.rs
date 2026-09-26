use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};

use axe_artifact::Target;
use goblin::Object;

pub fn validate_executable(path: &Path, target: Target, smoke: bool) -> Result<bool, String> {
    let metadata = fs::metadata(path)
        .map_err(|error| format!("inspect executable {}: {error}", path.display()))?;
    if !metadata.is_file() || metadata.len() == 0 {
        return Err(format!(
            "{} is not a non-empty regular file",
            path.display()
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            return Err(format!("{} is not executable", path.display()));
        }
    }

    let bytes = fs::read(path).map_err(|error| format!("read {}: {error}", path.display()))?;
    if contains(&bytes, b"/nix/store/") {
        return Err(format!(
            "{} contains a /nix/store runtime reference",
            path.display()
        ));
    }

    let fully_static = match Object::parse(&bytes)
        .map_err(|error| format!("parse executable {}: {error}", path.display()))?
    {
        Object::Elf(elf) => {
            validate_elf(&elf, target)?;
            elf.interpreter.is_none() && elf.libraries.is_empty()
        }
        Object::Mach(mach) => {
            validate_mach(&mach, target)?;
            false
        }
        _ => {
            return Err(format!(
                "{} is not an ELF or Mach-O executable",
                path.display()
            ));
        }
    };

    if smoke && target == Target::detect().map_err(|error| error.to_string())? {
        let status = Command::new(path)
            .arg("--help")
            .env_clear()
            .env("PATH", "")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|error| format!("smoke-run {}: {error}", path.display()))?;
        if status.code().is_none() {
            return Err(format!("smoke-run {} terminated by signal", path.display()));
        }
    }

    Ok(fully_static && matches!(target, Target::X86_64Linux | Target::Aarch64Linux))
}

fn validate_elf(elf: &goblin::elf::Elf<'_>, target: Target) -> Result<(), String> {
    let expected = match target {
        Target::X86_64Linux => goblin::elf::header::EM_X86_64,
        Target::Aarch64Linux => goblin::elf::header::EM_AARCH64,
        _ => return Err(format!("ELF payload is invalid for target {target}")),
    };
    if elf.header.e_machine != expected {
        return Err(format!(
            "ELF machine {} does not match {target}",
            elf.header.e_machine
        ));
    }
    Ok(())
}

fn validate_mach(mach: &goblin::mach::Mach<'_>, target: Target) -> Result<(), String> {
    let expected = match target {
        Target::Aarch64Darwin => goblin::mach::constants::cputype::CPU_TYPE_ARM64,
        _ => return Err(format!("Mach-O payload is invalid for target {target}")),
    };
    match mach {
        goblin::mach::Mach::Binary(binary) => validate_macho_binary(binary, expected),
        goblin::mach::Mach::Fat(fat) => {
            for architecture in fat.iter_arches() {
                let architecture =
                    architecture.map_err(|error| format!("parse fat Mach-O: {error}"))?;
                if architecture.cputype() == expected {
                    return Ok(());
                }
            }
            Err(format!(
                "fat Mach-O does not contain architecture for {target}"
            ))
        }
    }
}

fn validate_macho_binary(binary: &goblin::mach::MachO<'_>, expected: u32) -> Result<(), String> {
    if binary.header.cputype != expected {
        return Err("Mach-O has the wrong architecture".into());
    }
    for library in &binary.libs {
        if library.starts_with("/nix/store/")
            || (library.starts_with('/')
                && !library.starts_with("/usr/lib/")
                && !library.starts_with("/System/Library/"))
        {
            return Err(format!(
                "Mach-O has non-portable library reference {library}"
            ));
        }
    }
    Ok(())
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}
