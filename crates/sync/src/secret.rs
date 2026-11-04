//! Responsibility: Keep a secret on this PC (the login password, kept so the PC can log in
//! again by itself after the server is set up again) so that a copy of the file taken to
//! another computer does not give it away.
//!
//! On Windows the secret is sealed with DPAPI for this machine (`CryptProtectData` with
//! `CRYPTPROTECT_LOCAL_MACHINE`): only this Windows installation opens it. The file it is in
//! (`sync.json`) is also readable by SYSTEM and administrators only. Elsewhere (Linux, for
//! development and tests) it is only base64.

use base64::engine::general_purpose::STANDARD;
use base64::Engine;

/// The secret, sealed for this machine, as text.
pub fn protect(secret: &str) -> Result<String, String> {
    #[cfg(windows)]
    {
        Ok(format!(
            "dpapi:{}",
            STANDARD.encode(windows::protect(secret.as_bytes())?)
        ))
    }
    #[cfg(not(windows))]
    {
        Ok(format!("plain:{}", STANDARD.encode(secret.as_bytes())))
    }
}

/// The secret [`protect`] sealed.
pub fn unprotect(stored: &str) -> Result<String, String> {
    let bytes = if let Some(rest) = stored.strip_prefix("plain:") {
        STANDARD.decode(rest).map_err(|error| error.to_string())?
    } else if let Some(rest) = stored.strip_prefix("dpapi:") {
        let sealed = STANDARD.decode(rest).map_err(|error| error.to_string())?;
        #[cfg(windows)]
        {
            windows::unprotect(&sealed)?
        }
        #[cfg(not(windows))]
        {
            let _ = sealed;
            return Err("this secret was sealed on Windows".into());
        }
    } else {
        return Err("not a kept secret".into());
    };
    String::from_utf8(bytes).map_err(|error| error.to_string())
}

#[cfg(windows)]
mod windows {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CryptProtectData, CryptUnprotectData, CRYPTPROTECT_LOCAL_MACHINE,
        CRYPTPROTECT_UI_FORBIDDEN, CRYPT_INTEGER_BLOB,
    };

    fn run(
        input: &[u8],
        call: impl FnOnce(*const CRYPT_INTEGER_BLOB, *mut CRYPT_INTEGER_BLOB) -> i32,
    ) -> Result<Vec<u8>, String> {
        let input_blob = CRYPT_INTEGER_BLOB {
            cbData: input.len() as u32,
            pbData: input.as_ptr() as *mut u8,
        };
        let mut output_blob = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: std::ptr::null_mut(),
        };
        if call(&input_blob, &mut output_blob) == 0 {
            return Err(format!("DPAPI: {}", std::io::Error::last_os_error()));
        }
        // SAFETY: on success DPAPI allocated `cbData` bytes at `pbData`, freed with LocalFree.
        let output = unsafe {
            let bytes = std::slice::from_raw_parts(output_blob.pbData, output_blob.cbData as usize)
                .to_vec();
            LocalFree(output_blob.pbData as _);
            bytes
        };
        Ok(output)
    }

    pub fn protect(secret: &[u8]) -> Result<Vec<u8>, String> {
        run(secret, |input, output| {
            // SAFETY: the blobs live for the call; no description, entropy or prompt.
            unsafe {
                CryptProtectData(
                    input,
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null(),
                    CRYPTPROTECT_LOCAL_MACHINE | CRYPTPROTECT_UI_FORBIDDEN,
                    output,
                )
            }
        })
    }

    pub fn unprotect(sealed: &[u8]) -> Result<Vec<u8>, String> {
        run(sealed, |input, output| {
            // SAFETY: as in `protect`.
            unsafe {
                CryptUnprotectData(
                    input,
                    std::ptr::null_mut(),
                    std::ptr::null(),
                    std::ptr::null(),
                    std::ptr::null(),
                    CRYPTPROTECT_UI_FORBIDDEN,
                    output,
                )
            }
        })
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_kept_secret_comes_back() {
        let kept = super::protect("long password").unwrap();
        assert!(!kept.contains("long password"));
        assert_eq!(super::unprotect(&kept).unwrap(), "long password");
        assert!(super::unprotect("other").is_err());
    }
}
