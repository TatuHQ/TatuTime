use std::path::PathBuf;

use uuid::Uuid;

use super::io::{read_json, write_json};
use super::*;
use crate::domain::Notification;

impl Store {
    // ---------------------------------------------------------------- timers --

    pub fn get_timer(&self, user_id: Uuid) -> Result<Option<Timer>, StoreError> {
        self.get_doc::<Timer>(&user_id.to_string())
    }

    pub fn put_timer(&self, timer: &Timer) -> Result<(), StoreError> {
        self.put_doc(timer)
    }

    pub fn delete_timer(&self, user_id: Uuid) -> Result<(), StoreError> {
        let _guard = self.write_lock()?;
        let path = self.doc_path::<Timer>(&user_id.to_string());
        if path.exists() {
            std::fs::remove_file(&path)?;
        }
        Ok(())
    }

    // -------------------------------------------------------- notifications --

    fn notifications_path(&self, user_id: Uuid) -> PathBuf {
        self.root
            .join(NOTIFICATIONS_DIR)
            .join(format!("{user_id}.json"))
    }

    pub fn list_notifications(&self, user_id: Uuid) -> Result<Vec<Notification>, StoreError> {
        Ok(read_json::<Vec<Notification>>(&self.notifications_path(user_id))?.unwrap_or_default())
    }

    pub fn push_notification(&self, user_id: Uuid, n: &Notification) -> Result<(), StoreError> {
        let _guard = self.write_lock()?;
        let mut list: Vec<Notification> =
            read_json(&self.notifications_path(user_id))?.unwrap_or_default();
        list.push(n.clone());
        // Bound the stored list (newest kept).
        if list.len() > 200 {
            let start = list.len() - 200;
            list = list[start..].to_vec();
        }
        std::fs::create_dir_all(self.root.join(NOTIFICATIONS_DIR))?;
        write_json(&self.notifications_path(user_id), &list)
    }

    pub fn mark_notifications_read(&self, user_id: Uuid) -> Result<(), StoreError> {
        let _guard = self.write_lock()?;
        let mut list: Vec<Notification> =
            read_json(&self.notifications_path(user_id))?.unwrap_or_default();
        for n in list.iter_mut() {
            n.read = true;
        }
        std::fs::create_dir_all(self.root.join(NOTIFICATIONS_DIR))?;
        write_json(&self.notifications_path(user_id), &list)
    }

    // ------------------------------------------------------------- schedules --

    pub fn list_schedules(&self) -> Result<Vec<RecurringSchedule>, StoreError> {
        let mut out = self.list_docs::<RecurringSchedule>()?;
        out.sort_by_key(|s| s.created_at);
        Ok(out)
    }

    pub fn get_schedule(&self, id: Uuid) -> Result<Option<RecurringSchedule>, StoreError> {
        self.get_doc::<RecurringSchedule>(&id.to_string())
    }

    pub fn put_schedule(&self, s: &RecurringSchedule) -> Result<(), StoreError> {
        self.put_doc(s)
    }

    pub fn delete_schedule(&self, id: Uuid) -> Result<(), StoreError> {
        self.remove_doc::<RecurringSchedule>(&id.to_string())
    }
}
