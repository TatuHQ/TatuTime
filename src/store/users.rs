use std::collections::HashMap;

use uuid::Uuid;

use super::io::{dir_entries, read_json, write_json};
use super::*;

impl Store {
    // ---------------------------------------------------------------- users --

    pub fn list_users(&self) -> Result<Vec<User>, StoreError> {
        let mut out = self.list_docs::<User>()?;
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    /// Whether at least one user document **decodes to a valid `User`**.
    ///
    /// A file under `users/` is not proof of an account: a half-restored or
    /// hand-edited document is logged (file + reason) and skipped, so one
    /// corrupt file can no longer make the instance look initialised while
    /// leaving nobody able to sign in (bootstrap refused as `initialised`,
    /// every login scan failing with an opaque 500). Dot files such as the
    /// email side index and `*.tmp` leftovers are not documents (`dir_entries`
    /// skips them), and the #256 directory scan — not a decode of the whole
    /// collection — still answers the first candidate that validates.
    pub fn has_users(&self) -> Result<bool, StoreError> {
        let dir = self.doc_dir::<User>();
        if !dir.exists() {
            return Ok(false);
        }
        for path in dir_entries(&dir)? {
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            match read_json::<User>(&path) {
                Ok(Some(_)) => return Ok(true),
                Ok(None) => {}
                // Server log only: the client never learns a path or a parse
                // error (the one error shape stays generic).
                Err(error) => tracing::warn!(
                    file = %path.display(),
                    %error,
                    "skipping undecodable user document"
                ),
            }
        }
        Ok(false)
    }

    pub fn get_user(&self, id: Uuid) -> Result<Option<User>, StoreError> {
        self.get_doc::<User>(&id.to_string())
    }

    pub fn get_user_by_email(&self, email: &str) -> Result<Option<User>, StoreError> {
        // Fast path: the email side index, verified against the document.
        if let Some(id) =
            read_json::<HashMap<String, String>>(&self.email_index_path())?.and_then(|idx| {
                idx.get(&email.to_lowercase())
                    .and_then(|s| Uuid::parse_str(s).ok())
            })
            && let Some(user) = self.get_user(id)?
            && user.email.eq_ignore_ascii_case(email)
        {
            return Ok(Some(user));
        }
        // The scan stays authoritative (pre-index dirs, restored docs, drift).
        let found = self
            .list_users()?
            .into_iter()
            .find(|u| u.email.eq_ignore_ascii_case(email));
        // Reaching a hit through the scan means the fast path missed while a
        // matching document exists: the index is absent or stale — heal it.
        if found.is_some() {
            self.rebuild_email_index()?;
        }
        Ok(found)
    }

    pub fn put_user(&self, user: &User) -> Result<(), StoreError> {
        let _guard = self.write_lock()?;
        self.put_user_locked(user)
    }

    pub fn put_user_if_unchanged(&self, user: &User, expected: &User) -> Result<(), StoreError> {
        let _guard = self.write_lock()?;
        let current = self.get_user(user.id)?.ok_or(StoreError::NotFound)?;
        // #256: `User` is `PartialEq`; compare the documents directly rather
        // than round-tripping both through serde (which serialised two argon2
        // hash strings) purely as an equality check.
        if current != *expected {
            return Err(StoreError::Conflict(
                "user changed; reload and try again".into(),
            ));
        }
        self.put_user_locked(user)
    }

    fn put_user_locked(&self, user: &User) -> Result<(), StoreError> {
        let users = self.list_users()?;
        if users
            .iter()
            .any(|other| other.id != user.id && other.email.eq_ignore_ascii_case(&user.email))
        {
            return Err(StoreError::Conflict("a user with that email exists".into()));
        }
        if users.iter().any(|other| other.id == user.id)
            && !(user.active && user.role == crate::auth::Role::Admin)
            && !users.iter().any(|other| {
                other.id != user.id && other.active && other.role == crate::auth::Role::Admin
            })
        {
            return Err(StoreError::Conflict(
                "at least one active administrator is required".into(),
            ));
        }
        // QA-05: a *new* account is subject to the per-collection cap; an
        // update of an existing one is not.
        if !self.doc_path::<User>(&user.id.to_string()).exists() {
            self.ensure_capacity::<User>()?;
        }
        std::fs::create_dir_all(self.doc_dir::<User>())?;
        write_json(&self.doc_path::<User>(&user.id.to_string()), user)?;
        self.index_user_locked(user)
    }

    pub fn delete_user(&self, id: Uuid) -> Result<(), StoreError> {
        let _guard = self.write_lock()?;
        let target = self.get_user(id)?.ok_or(StoreError::NotFound)?;
        if target.active
            && target.role == crate::auth::Role::Admin
            && !self
                .list_users()?
                .iter()
                .any(|user| user.id != id && user.active && user.role == crate::auth::Role::Admin)
        {
            return Err(StoreError::Conflict(
                "at least one active administrator is required".into(),
            ));
        }
        if self
            .list_all_entries()?
            .iter()
            .any(|entry| entry.user_id == Some(id))
            || self
                .list_expenses()?
                .iter()
                .any(|expense| expense.user_id == Some(id))
            || self
                .list_submissions()?
                .iter()
                .any(|submission| submission.user_id == id)
            || self.list_claims()?.iter().any(|claim| claim.user_id == id)
            || self.get_timer(id)?.is_some()
        {
            return Err(StoreError::Conflict(
                "user has recorded history or a running timer; deactivate instead".into(),
            ));
        }
        self.remove_doc_locked::<User>(&id.to_string())?;
        self.unindex_user_locked(&id.to_string())
    }

    // -------------------------------------------------------- item types --

    pub fn list_item_types(&self) -> Result<Vec<crate::domain::ItemType>, StoreError> {
        let mut out = self.list_docs::<crate::domain::ItemType>()?;
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    pub fn get_item_type(&self, id: Uuid) -> Result<Option<crate::domain::ItemType>, StoreError> {
        self.get_doc::<crate::domain::ItemType>(&id.to_string())
    }

    pub fn put_item_type(&self, item: &crate::domain::ItemType) -> Result<(), StoreError> {
        self.put_doc(item)
    }

    pub fn delete_item_type(&self, id: Uuid) -> Result<(), StoreError> {
        self.remove_doc::<crate::domain::ItemType>(&id.to_string())
    }

    // ---------------------------------------------------------- retainers --

    pub fn list_retainers(&self) -> Result<Vec<crate::retainer::Retainer>, StoreError> {
        let mut out = self.list_docs::<crate::retainer::Retainer>()?;
        out.sort_by_key(|r| r.created_at);
        Ok(out)
    }

    pub fn get_retainer(&self, id: Uuid) -> Result<Option<crate::retainer::Retainer>, StoreError> {
        self.get_doc::<crate::retainer::Retainer>(&id.to_string())
    }

    pub fn put_retainer(&self, r: &crate::retainer::Retainer) -> Result<(), StoreError> {
        self.put_doc(r)
    }

    /// Read-modify-write a retainer's ledger under the store lock (#144):
    /// two concurrent operations can never interleave a half-applied tx.
    /// The closure's own error type survives intact (#187) — erasing it into
    /// `StoreError::Io(String)` forced callers to re-parse messages by
    /// substring, and any path that forgot to was answering 500 to a 409.
    pub fn update_retainer<F, E>(
        &self,
        id: Uuid,
        f: F,
    ) -> Result<crate::retainer::Retainer, UpdateError<E>>
    where
        F: FnOnce(&mut crate::retainer::Retainer) -> Result<(), E>,
    {
        let _guard = self.write_lock()?;
        let path = self.doc_path::<crate::retainer::Retainer>(&id.to_string());
        let Some(mut r) = read_json::<crate::retainer::Retainer>(&path)? else {
            return Err(StoreError::NotFound.into());
        };
        f(&mut r).map_err(UpdateError::Domain)?;
        write_json(&path, &r)?;
        Ok(r)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn has_users_ignores_side_indexes_and_tmp_files() {
        // #256: the public status probe answers from the directory, so only a
        // real user *document* may flip it.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("data");
        let store = Store::open(&root).unwrap();
        // No users dir at all.
        assert!(!store.has_users().unwrap());
        // The email side index and a stranded `.tmp` write live in the users
        // dir but are not documents.
        let users_dir = root.join("users");
        std::fs::create_dir_all(&users_dir).unwrap();
        std::fs::write(users_dir.join(".idx.emails.json"), b"{}").unwrap();
        std::fs::write(users_dir.join("abc.json.tmp"), b"{}").unwrap();
        assert!(!store.has_users().unwrap());
        // A `*.json` document that does not decode to a `User` is not an
        // account: the probe skips it (and logs it) instead of claiming the
        // instance is initialised (the lockout this guards against).
        std::fs::write(users_dir.join("broken.json"), b"not json").unwrap();
        assert!(!store.has_users().unwrap());
        // A real document flips it. Written directly because this test is
        // about the scan: `put_user` itself scans the collection (and a
        // corrupt sibling is a store error on that path).
        let user = User {
            id: Uuid::new_v4(),
            name: "A".into(),
            email: "a@example.com".into(),
            role: crate::auth::Role::Member,
            active: true,
            default_rate_minor: 0,
            cost_rate_minor: 0,
            password_hash: "x".into(),
            created_at: chrono::Utc::now(),
            session_version: 0,
        };
        std::fs::write(
            users_dir.join(format!("{}.json", user.id)),
            serde_json::to_vec(&user).unwrap(),
        )
        .unwrap();
        assert!(store.has_users().unwrap());
    }
}
