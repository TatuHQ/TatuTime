use uuid::Uuid;

use super::io::write_json;
use super::*;

impl Store {
    // ----------------------------------------------------------- categories --

    pub fn list_categories(&self) -> Result<Vec<Category>, StoreError> {
        let mut out = self.list_docs::<Category>()?;
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    /// Seed common expense categories once for a fresh installation. The
    /// marker prevents deleted defaults from reappearing on a later restart.
    pub fn ensure_default_categories(&self) -> Result<(), StoreError> {
        let marker = self.doc_dir::<Category>().join(".defaults-seeded");
        if marker.exists() {
            return Ok(());
        }
        let _guard = self.write_lock()?;
        if marker.exists() {
            return Ok(());
        }

        let existing = self.list_docs::<Category>()?;
        let dir = self.doc_dir::<Category>();
        std::fs::create_dir_all(&dir)?;
        if existing.is_empty() {
            for name in ["Food", "Travel", "Lodging", "Supplies", "Software"] {
                let category = Category {
                    id: Uuid::new_v4(),
                    name: name.to_string(),
                    default_billable: true,
                    active: true,
                };
                write_json(
                    &self.doc_path::<Category>(&category.id.to_string()),
                    &category,
                )?;
            }
        }

        let marker_tmp = dir.join(".defaults-seeded.tmp");
        std::fs::write(&marker_tmp, b"expense-category-defaults-v1\n")?;
        std::fs::rename(marker_tmp, marker)?;
        Ok(())
    }

    pub fn get_category(&self, id: Uuid) -> Result<Option<Category>, StoreError> {
        self.get_doc::<Category>(&id.to_string())
    }

    pub fn put_category(&self, category: &Category) -> Result<(), StoreError> {
        self.put_doc(category)
    }

    pub fn update_category(&self, category: &Category) -> Result<(), StoreError> {
        let _guard = self.write_lock()?;
        let path = self.doc_path::<Category>(&category.id.to_string());
        if !path.exists() {
            return Err(StoreError::NotFound);
        }
        write_json(&path, category)
    }

    pub fn delete_category(&self, id: Uuid) -> Result<(), StoreError> {
        let _guard = self.write_lock()?;
        if !self.doc_path::<Category>(&id.to_string()).exists() {
            return Err(StoreError::NotFound);
        }
        if self
            .list_expenses()?
            .iter()
            .any(|e| e.category_id == Some(id))
        {
            // A record still referenced by others is a *state conflict*, not an
            // id collision, so `Conflict` is the honest variant at this seam.
            // The full typed-error-family consolidation is deferred to #187.
            return Err(StoreError::Conflict(
                "category still used by expenses".into(),
            ));
        }
        self.remove_doc_locked::<Category>(&id.to_string())
    }

    // ------------------------------------------------------------- expenses --

    pub fn list_expenses(&self) -> Result<Vec<Expense>, StoreError> {
        let mut out = self.walk_docs(&self.doc_dir::<Expense>(), |p| self.read_expense(p))?;
        out.sort_by_key(|e| std::cmp::Reverse((e.date, e.created_at)));
        Ok(out)
    }

    pub fn get_expense(&self, id: Uuid) -> Result<Option<Expense>, StoreError> {
        self.get_doc::<Expense>(&id.to_string())
    }

    /// Lock, ensure the directory, atomic write — and file the row in the
    /// per-user index (#335) under the same lock, so a crash can only strand a
    /// stale index (safe: reads verify it) and never a phantom row.
    pub fn put_expense(&self, expense: &Expense) -> Result<(), StoreError> {
        let _guard = self.write_lock()?;
        let path = self.doc_path::<Expense>(&expense.id.to_string());
        if !path.exists() {
            self.ensure_capacity::<Expense>()?;
        }
        std::fs::create_dir_all(self.doc_dir::<Expense>())?;
        write_json(&path, expense)?;
        self.index_expense_locked(expense)
    }

    pub fn delete_expense(&self, id: Uuid) -> Result<(), StoreError> {
        let _guard = self.write_lock()?;
        self.remove_doc_locked::<Expense>(&id.to_string())?;
        self.unindex_expense_locked(&id.to_string())
    }

    // ---------------------------------------------------------- submissions --

    pub fn list_submissions(&self) -> Result<Vec<Submission>, StoreError> {
        let mut out = self.list_docs::<Submission>()?;
        out.sort_by_key(|s| std::cmp::Reverse(s.week_start));
        Ok(out)
    }

    pub fn get_submission(&self, id: Uuid) -> Result<Option<Submission>, StoreError> {
        self.get_doc::<Submission>(&id.to_string())
    }

    pub fn put_submission(&self, submission: &Submission) -> Result<(), StoreError> {
        self.put_doc(submission)
    }

    // --------------------------------------------------------------- claims --

    pub fn list_claims(&self) -> Result<Vec<ExpenseClaim>, StoreError> {
        let mut out = self.list_docs::<ExpenseClaim>()?;
        out.sort_by_key(|c| std::cmp::Reverse(c.created_at));
        Ok(out)
    }

    pub fn get_claim(&self, id: Uuid) -> Result<Option<ExpenseClaim>, StoreError> {
        self.get_doc::<ExpenseClaim>(&id.to_string())
    }

    pub fn put_claim(&self, claim: &ExpenseClaim) -> Result<(), StoreError> {
        self.put_doc(claim)
    }
}
