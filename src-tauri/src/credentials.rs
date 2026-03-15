use keyring::Entry;

const SERVICE_NAME: &str = "meerkat";
const USERNAME: &str = "gitlab-pat";

pub fn store_token(token: &str) -> Result<(), String> {
    let entry = Entry::new(SERVICE_NAME, USERNAME).map_err(|e| format!("Keyring error: {e}"))?;
    entry
        .set_password(token)
        .map_err(|e| format!("Failed to store token: {e}"))
}

pub fn get_token() -> Result<Option<String>, String> {
    let entry = Entry::new(SERVICE_NAME, USERNAME).map_err(|e| format!("Keyring error: {e}"))?;
    match entry.get_password() {
        Ok(token) => Ok(Some(token)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(format!("Failed to retrieve token: {e}")),
    }
}

pub fn delete_token() -> Result<(), String> {
    let entry = Entry::new(SERVICE_NAME, USERNAME).map_err(|e| format!("Keyring error: {e}"))?;
    match entry.delete_credential() {
        Ok(()) => Ok(()),
        Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(format!("Failed to delete token: {e}")),
    }
}
