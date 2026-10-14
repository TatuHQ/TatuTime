// notifications.js — TucanoTime GUI module (ES-module split of web/app.js, #264).
// Moved from the original monolithic app.js; behaviour is unchanged.


import { api } from './api.js';
import { $, announce, el } from './dom.js';

// ------------------------------------------------------------ notifications --

export async function refreshNotifications() {
  try {
    const data = await api.get('/notifications');
    const list = data.notifications || [];
    const unread = list.filter((n) => !n.read);
    $('notif-section').hidden = unread.length === 0;
    const ul = $('notif-list');
    ul.textContent = '';
    for (const n of unread) {
      ul.appendChild(
        el('li', { cls: 'notif p-notification p-notification--information' }, [
          el('strong', { text: n.title }),
          el('span', { text: ` ${n.body}` }),
        ]),
      );
    }
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

