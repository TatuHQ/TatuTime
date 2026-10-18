use std::path::{Path, PathBuf};

use uuid::Uuid;

use super::io::{read_json, write_json};
use super::*;
use crate::domain::{Project, Task};

impl Store {
    // ------------------------------------------------------------ customers --

    pub fn list_customers(&self) -> Result<Vec<Customer>, StoreError> {
        let mut out = self.list_docs::<Customer>()?;
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    pub fn get_customer(&self, id: Uuid) -> Result<Option<Customer>, StoreError> {
        self.get_doc::<Customer>(&id.to_string())
    }

    pub fn put_customer(&self, customer: &Customer) -> Result<(), StoreError> {
        self.put_doc(customer)
    }

    pub fn delete_customer(&self, id: Uuid) -> Result<(), StoreError> {
        let _guard = self.write_lock()?;
        // A customer owns its project folder; removing it would orphan those
        // documents, so the caller must empty it first. An unreadable project
        // set must NOT read as "empty" (#185): propagating here keeps the
        // remove_dir_all below from deleting documents we failed to list.
        let projects = self.list_projects(id)?;
        if !projects.is_empty() {
            return Err(StoreError::Conflict(
                "customer still has projects; delete them first".into(),
            ));
        }
        if self.has_entries_for_customer(id)? {
            return Err(StoreError::Conflict(
                "customer still has time entries; delete or re-point them first".into(),
            ));
        }
        let doc = self.doc_path::<Customer>(&id.to_string());
        if !doc.exists() {
            return Err(StoreError::NotFound);
        }
        // The projects live in a sibling DIRECTORY (customers/<id>/projects),
        // which exists only once a project was ever added; ENOENT here must not
        // fail the deletion of a project-less customer (#141).
        let dir = self.root.join("customers").join(id.to_string());
        match std::fs::remove_dir_all(&dir) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(StoreError::Io(e.to_string())),
        }
        std::fs::remove_file(&doc)?;
        Ok(())
    }

    // ------------------------------------------------------------- projects --

    fn projects_dir(&self, customer_id: Uuid) -> PathBuf {
        self.root
            .join("customers")
            .join(customer_id.to_string())
            .join("projects")
    }

    fn project_path(&self, customer_id: Uuid, code: &str) -> PathBuf {
        self.projects_dir(customer_id).join(format!("{code}.json"))
    }

    pub fn list_projects(&self, customer_id: Uuid) -> Result<Vec<Project>, StoreError> {
        let customer = self
            .get_customer(customer_id)?
            .ok_or(StoreError::NotFound)?;
        // The nested-collection case (#101): parent key + injected decode hook
        // (`read_project` resolves pre-#11 documents from the customer).
        let mut out = self.walk_docs(&self.projects_dir(customer_id), |path| {
            self.read_project(path, &customer)
        })?;
        out.sort_by(|a, b| a.code.cmp(&b.code));
        Ok(out)
    }

    /// Read one project document, resolving any pre-#11 missing currency/rate
    /// from the owning customer.
    fn read_project(
        &self,
        path: &Path,
        customer: &Customer,
    ) -> Result<Option<Project>, StoreError> {
        match std::fs::read(path) {
            Ok(bytes) => {
                let project = crate::domain::project_from_bytes(&bytes, customer)
                    .map_err(|e| StoreError::Io(format!("corrupt project document: {e}")))?;
                Ok(Some(project))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    pub fn get_project(
        &self,
        customer_id: Uuid,
        code: &str,
    ) -> Result<Option<Project>, StoreError> {
        let customer = self
            .get_customer(customer_id)?
            .ok_or(StoreError::NotFound)?;
        self.read_project(&self.project_path(customer_id, code), &customer)
    }

    pub fn put_project(&self, project: &Project) -> Result<(), StoreError> {
        let _guard = self.write_lock()?;
        std::fs::create_dir_all(self.projects_dir(project.customer_id))?;
        write_json(
            &self.project_path(project.customer_id, &project.code.0),
            project,
        )
    }

    pub fn delete_project(&self, customer_id: Uuid, code: &str) -> Result<(), StoreError> {
        let _guard = self.write_lock()?;
        let path = self.project_path(customer_id, code);
        if !path.exists() {
            return Err(StoreError::NotFound);
        }
        if !self.list_tasks(customer_id, code)?.is_empty() {
            return Err(StoreError::Conflict(
                "project still has tasks; delete them first".into(),
            ));
        }
        if self.has_entries_for_project(customer_id, code)? {
            return Err(StoreError::Conflict(
                "project still has time entries; delete or re-point them first".into(),
            ));
        }
        std::fs::remove_file(&path)?;
        // Remove the project's task folder (tasks live inside it).
        let _ = std::fs::remove_dir_all(self.tasks_dir(customer_id, code));
        Ok(())
    }

    // ---------------------------------------------------------------- tasks --

    fn tasks_dir(&self, customer_id: Uuid, project_code: &str) -> PathBuf {
        self.projects_dir(customer_id)
            .join(project_code)
            .join("tasks")
    }

    fn task_path(&self, customer_id: Uuid, project_code: &str, code: &str) -> PathBuf {
        self.tasks_dir(customer_id, project_code)
            .join(format!("{code}.json"))
    }

    pub fn list_tasks(
        &self,
        customer_id: Uuid,
        project_code: &str,
    ) -> Result<Vec<Task>, StoreError> {
        let mut out = self.walk_docs(
            &self.tasks_dir(customer_id, project_code),
            read_json::<Task>,
        )?;
        out.sort_by(|a, b| a.code.cmp(&b.code));
        Ok(out)
    }

    pub fn get_task(
        &self,
        customer_id: Uuid,
        project_code: &str,
        code: &str,
    ) -> Result<Option<Task>, StoreError> {
        read_json(&self.task_path(customer_id, project_code, code))
    }

    pub fn put_task(&self, task: &Task) -> Result<(), StoreError> {
        let _guard = self.write_lock()?;
        std::fs::create_dir_all(self.tasks_dir(task.customer_id, &task.project_code.0))?;
        write_json(
            &self.task_path(task.customer_id, &task.project_code.0, &task.code.0),
            task,
        )
    }

    pub fn delete_task(
        &self,
        customer_id: Uuid,
        project_code: &str,
        code: &str,
    ) -> Result<(), StoreError> {
        let _guard = self.write_lock()?;
        let path = self.task_path(customer_id, project_code, code);
        if !path.exists() {
            return Err(StoreError::NotFound);
        }
        if self.has_entries_for_task(customer_id, project_code, code)? {
            return Err(StoreError::Conflict(
                "task still has time entries; delete or re-point them first".into(),
            ));
        }
        std::fs::remove_file(&path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cust() -> Customer {
        Customer {
            id: Uuid::new_v4(),
            name: "X".into(),
            currency: crate::domain::Currency("EUR".into()),
            default_rate_minor: 1,
            active: true,
            email: String::new(),
            payment_terms: None,
            invoice_notes: String::new(),
            invoice_subject: String::new(),
            address: None,
            contacts: vec![],
            tax_hundredths: 0,
            discount_hundredths: 0,
            payment_details: None,
        }
    }

    #[test]
    fn collection_cap_is_enforced_on_write_not_on_read() {
        // QA-05: the cap refuses the *new* document before it is persisted and
        // the collection stays readable — an over-cap directory is never
        // unreadable, and a write never lands past the cap.
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("data"))
            .unwrap()
            .with_max_docs(2);
        store.put_customer(&cust()).unwrap();
        store.put_customer(&cust()).unwrap();
        let err = store
            .put_customer(&cust())
            .expect_err("3rd file > cap of 2");
        assert!(matches!(err, StoreError::TooManyItems), "{err:?}");
        assert_eq!(
            store.list_customers().unwrap().len(),
            2,
            "the refused write persisted nothing and the collection still lists"
        );
        // An update of an existing document is not a new member: still allowed.
        let existing = store.list_customers().unwrap();
        let mut updated = existing[0].clone();
        updated.name = "Renamed".into();
        store
            .put_customer(&updated)
            .expect("overwrite stays allowed");
    }

    #[test]
    fn over_cap_directory_still_lists_everything() {
        // QA-05: a pre-seeded over-cap directory (an operator restored it, or
        // an older build wrote past the cap) must stay recoverable.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("data");
        let store = Store::open(&root).unwrap();
        for _ in 0..3 {
            store.put_customer(&cust()).unwrap();
        }
        let capped = Store::open(&root).unwrap().with_max_docs(2);
        assert_eq!(
            capped.list_customers().expect("over-cap read lists").len(),
            3
        );
    }
}
