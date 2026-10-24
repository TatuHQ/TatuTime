// notifications.js — TatuTime GUI module (ES-module split of web/app.js, #264).
// Moved from the original monolithic app.js; behaviour is unchanged.


import { api } from './api.js';
import { $, announce, el } from './dom.js';

// ------------------------------------------------------------ notifications --

// #232 increment 3: the backend's notifications carry a `kind`, and every kind it
// emits today (`no_time_today`) is informational — so this keeps the current
// severity while wiring the official Vanilla notification variants for future
// kinds. Announcements are a separate, unchanged surface: the polite global
// `#live-region` (`role="status"`) and the field-level `role="alert"` errors.
const NOTIFICATION_SEVERITY = {
  no_time_today: 'information',
};

export function notificationSeverity(kind) {
  return NOTIFICATION_SEVERITY[kind] || 'information';
}

// Canonical Vanilla notification: `p-notification--<severity>` wrapping a
// `p-notification__content` that holds the title and message. Values enter the
// DOM through `textContent` only.
export function notificationNode(notification) {
  const content = [el('p', { cls: 'p-notification__title', text: notification.title })];
  if (notification.body) {
    content.push(el('p', { cls: 'p-notification__message', text: notification.body }));
  }
  return el(
    'li',
    { cls: `p-notification p-notification--${notificationSeverity(notification.kind)}` },
    [el('div', { cls: 'p-notification__content' }, content)],
  );
}

export async function refreshNotifications() {
  try {
    const data = await api.get('/notifications');
    const list = data.notifications || [];
    const unread = list.filter((n) => !n.read);
    $('notif-section').hidden = unread.length === 0;
    const ul = $('notif-list');
    ul.textContent = '';
    for (const n of unread) ul.appendChild(notificationNode(n));
  } catch {
    $('notif-section').hidden = true;
  }
}

export async function markNotificationsRead() {
  try {
    await api.post('/notifications/read');
    await refreshNotifications();
  } catch (err) {
    announce(err.message);
  }
}
