//! `sb setup home`: directories, database, permissions and pseudo-accounts.

use sb_store::{Catalog, Home};
use second_brain_kernel::{AccountId, AccountKind};

use crate::error::{Result, SetupError};

/// What `setup home` did.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct HomeReport {
    pub home: String,
    pub created: bool,
    pub default_home: bool,
    pub pseudo_accounts_created: Vec<String>,
}

/// Create the home and the catalog, apply permissions, and create the `local`
/// and `web` pseudo-accounts. Idempotent.
pub fn setup_home(home: &Home) -> Result<(Catalog, HomeReport)> {
    let created = !home.is_initialized();
    let cat = Catalog::create(home)?;
    let mut pseudo = Vec::new();
    for (id, kind, label) in [
        ("local", AccountKind::Local, "Local files"),
        ("web", AccountKind::Web, "Web pages"),
    ] {
        let id = AccountId::new(id).map_err(|e| SetupError::Invalid(e.to_string()))?;
        if cat.ensure_account(&id, kind, label)? {
            pseudo.push(id.to_string());
        }
    }
    cat.secure_files()?;
    let report = HomeReport {
        home: home.root().display().to_string(),
        created,
        default_home: home.is_default(),
        pseudo_accounts_created: pseudo,
    };
    Ok((cat, report))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idempotent() {
        let d = tempfile::tempdir().unwrap();
        let home = Home::new(d.path().join("h"));
        let (_, r1) = setup_home(&home).unwrap();
        assert!(r1.created);
        assert_eq!(r1.pseudo_accounts_created, vec!["local", "web"]);
        let (cat, r2) = setup_home(&home).unwrap();
        assert!(!r2.created);
        assert!(r2.pseudo_accounts_created.is_empty());
        assert_eq!(cat.accounts().unwrap().len(), 2);
    }
}
