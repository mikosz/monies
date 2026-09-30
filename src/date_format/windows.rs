use windows_sys::Win32::Globalization::{GetLocaleInfoEx, LOCALE_SSHORTDATE};

/// The user's short date pattern in Windows notation, e.g. `d.MM.yyyy`.
pub fn short_date_pattern() -> Option<String> {
    // LOCALE_SSHORTDATE is at most 80 characters including the terminating null.
    let mut buffer = [0u16; 80];
    // SAFETY: a null locale name selects the user's default locale, and the buffer length
    // passed matches the buffer.
    let len = unsafe {
        GetLocaleInfoEx(std::ptr::null(), LOCALE_SSHORTDATE, buffer.as_mut_ptr(), buffer.len() as i32)
    };
    // `len` includes the terminating null; 0 means failure.
    let len = usize::try_from(len).ok().filter(|&len| len > 0)?;
    String::from_utf16(&buffer[..len - 1]).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_user_short_date_pattern() {
        let pattern = short_date_pattern().expect("short date pattern is available");
        println!("user short date pattern: {pattern:?}");
        assert!(!pattern.is_empty());
    }
}
