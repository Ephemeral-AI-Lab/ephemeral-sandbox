//! Minimal ustar stream builder for Docker archive uploads.

use anyhow::{bail, Context, Result};

pub(crate) fn tar_single_file(name: &str, payload: &[u8], mode: u32) -> Result<Vec<u8>> {
    if name.is_empty() || name.starts_with('/') || name.split('/').any(|part| part == "..") {
        bail!("invalid tar entry name {name:?}");
    }
    let name_bytes = name.as_bytes();
    if name_bytes.len() > 100 {
        bail!("tar entry name too long: {name}");
    }

    let mut header = [0_u8; 512];
    header[..name_bytes.len()].copy_from_slice(name_bytes);
    write_octal(&mut header[100..108], u64::from(mode))?;
    write_octal(&mut header[108..116], 0)?;
    write_octal(&mut header[116..124], 0)?;
    write_octal(&mut header[124..136], payload.len() as u64)?;
    write_octal(&mut header[136..148], 0)?;
    header[148..156].fill(b' ');
    header[156] = b'0';
    header[257..263].copy_from_slice(b"ustar\0");
    header[263..265].copy_from_slice(b"00");
    let checksum = header.iter().map(|byte| u32::from(*byte)).sum::<u32>();
    write_checksum(&mut header[148..156], checksum)?;

    let mut archive = Vec::with_capacity(512 + payload.len() + 1536);
    archive.extend_from_slice(&header);
    archive.extend_from_slice(payload);
    let padding = (512 - (payload.len() % 512)) % 512;
    archive.resize(archive.len() + padding, 0);
    archive.resize(archive.len() + 1024, 0);
    Ok(archive)
}

fn write_octal(field: &mut [u8], value: u64) -> Result<()> {
    let digits = field
        .len()
        .checked_sub(1)
        .context("tar octal field too short")?;
    let encoded = format!("{value:0width$o}", width = digits);
    if encoded.len() > digits {
        bail!(
            "tar octal value {value} does not fit in {} bytes",
            field.len()
        );
    }
    field[..digits].copy_from_slice(encoded.as_bytes());
    field[digits] = 0;
    Ok(())
}

fn write_checksum(field: &mut [u8], value: u32) -> Result<()> {
    if field.len() != 8 {
        bail!("tar checksum field must be 8 bytes");
    }
    let encoded = format!("{value:06o}");
    if encoded.len() > 6 {
        bail!("tar checksum {value} does not fit");
    }
    field[..6].copy_from_slice(encoded.as_bytes());
    field[6] = 0;
    field[7] = b' ';
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::tar_single_file;

    #[test]
    fn tar_single_file_builds_executable_ustar_stream() {
        let tar = tar_single_file("eosd", b"payload", 0o755).expect("tar stream");
        assert_eq!(&tar[0..4], b"eosd");
        assert_eq!(&tar[100..108], b"0000755\0");
        assert_eq!(&tar[124..136], b"00000000007\0");
        assert_eq!(tar[156], b'0');
        assert_eq!(&tar[257..263], b"ustar\0");
        assert_eq!(tar.len() % 512, 0);
    }
}
