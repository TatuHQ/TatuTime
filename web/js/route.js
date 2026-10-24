// route.js — TatuTime GUI module (ES-module split of web/app.js, #264).
// Moved from the original monolithic app.js; behaviour is unchanged.


import { refreshPanel, suppressPanelRefresh, switchTab } from './boot.js';
import { refreshTimeCal } from './calendar.js';
import { $ } from './dom.js';
import { releaseTemplatePreview, resumeTemplatePreview } from './settings.js';
import { refreshDay, refreshWeek } from './timesheet.js';

// #365: a tab's `focus()` scrolls it into view, which flushes layout. Module
// scripts run while `readyState` is still "loading", so focusing the selected
// tab from the boot-time route selection forced layout before the page was
// loaded — Firefox logs that as "Layout was forced before the page was fully
// loaded. If stylesheets are not yet loaded this may cause a flash of unstyled
// content." Move focus to the load event instead; a click after load still
// focuses synchronously.
function focusTab(tab) {
  if (!tab) return;
  if (tab.closest('#navigation-drawer:not([open])')) return;
  // Keyboard navigation inside a tab strip must move focus immediately (the
  // next key event is dispatched from the newly focused tab); only a focus move
  // that starts outside the strip — the boot-time route selection — is deferred.
  const strip = tab.closest('[role="tablist"]');
  const withinStrip = !!(strip && strip.contains(document.activeElement));
  if (document.readyState === 'complete' || withinStrip) {
    tab.focus();
    return;
  }
  window.addEventListener('load', () => {
    // The user may have selected another view in the meantime: only move focus
    // onto the tab that is still selected.
    if (tab.isConnected && tab.getAttribute('aria-selected') === 'true'
      && !tab.closest('#navigation-drawer:not([open])')) {
      tab.focus({ preventScroll: true });
    }
  }, { once: true });
}

// ------------------------------------------------------- timesheet segment --
//
// #107: Day and Week are views *inside* the Timesheets section — a second,
// horizontal tablist with its own roving tabindex (the sidebar list is
// scoped to #tabs, so the two never mix).

export let segActiveId = 'ts-day';

export function activateSeg(segId, refresh = true) {
  for (const id of ['ts-day', 'ts-week', 'ts-cal']) {
    const on = id === segId;
    const tab = $(id);
    tab.setAttribute('aria-selected', String(on));
    tab.tabIndex = on ? 0 : -1;
    $(tab.getAttribute('aria-controls')).hidden = !on;
  }
  segActiveId = segId;
  syncRoute();
  if (refresh) refreshTimesheetView();
}

export function refreshTimesheetView() {
  const run =
    segActiveId === 'ts-day' ? refreshDay()
      : segActiveId === 'ts-cal' ? refreshTimeCal()
        : refreshWeek();
  run.catch?.(() => { });
}

export function initSegTabs() {
  const segs = [$('ts-day'), $('ts-week'), $('ts-cal')];
  segs.forEach((t, i) => {
    t.addEventListener('click', () => activateSeg(t.id));
    t.addEventListener('keydown', (e) => {
      let j = null;
      if (e.key === 'ArrowRight' || e.key === 'ArrowLeft') j = (i + 1) % segs.length;
      if (e.key === 'Home') j = 0;
      if (e.key === 'End') j = segs.length - 1;
      if (j !== null) {
        e.preventDefault();
        activateSeg(segs[j].id);
        segs[j].focus();
      }
    });
  });
}

export function initSettingsTabs() {
  const tabs = Array.from(document.querySelectorAll('#settings-tabs [role="tab"]'));
  function activate(tab, moveFocus = true) {
    // #365: leaving the Invoice template section must drop its framed blob PDF
    // synchronously, in the same tick as the switch. The MutationObserver below
    // that watches `hidden` runs later (as a microtask), and a blob iframe
    // still loading while the next view boots is exactly what forced layout and
    // produced Firefox's "Layout was forced before the page was fully loaded"
    // warning on the following view.
    if (tab.getAttribute('aria-controls') !== 'settings-panel-invoice-template') {
      releaseTemplatePreview();
    }
    tabs.forEach((candidate) => {
      const active = candidate === tab;
      candidate.setAttribute('aria-selected', String(active));
      candidate.tabIndex = active ? 0 : -1;
      const panel = $(candidate.getAttribute('aria-controls'));
      if (panel) panel.hidden = !active;
    });
    if (moveFocus) focusTab(tab);
    syncRoute();
  }
  tabs.forEach((tab) => {
    tab.addEventListener('click', () => activate(tab, false));
    tab.addEventListener('keydown', (event) => {
      const visibleTabs = tabs.filter((candidate) => !candidate.hidden);
      const index = visibleTabs.indexOf(tab);
      let next = null;
      if (event.key === 'ArrowRight') next = (index + 1) % visibleTabs.length;
      if (event.key === 'ArrowLeft') next = (index - 1 + visibleTabs.length) % visibleTabs.length;
      if (event.key === 'Home') next = 0;
      if (event.key === 'End') next = visibleTabs.length - 1;
      if (next !== null) {
        event.preventDefault();
        activate(visibleTabs[next]);
      }
    });
  });
  const firstVisibleTab = tabs.find((tab) => !tab.hidden);
  if (firstVisibleTab) activate(firstVisibleTab, false);
}

// Opens the Timesheets section with a given segment (used by jump-to-entry
// and the week grid's "track time" flow).
export function showTimesheet(segId) {
  if ($('tab-timesheet').getAttribute('aria-selected') !== 'true') {
    suppressPanelRefresh = true;
    $('tab-timesheet').click();
    suppressPanelRefresh = false;
  }
  activateSeg(segId, false);
}

// ------------------------------------------------------------- url router ---
//
// Every view has a stable, case-insensitive path (e.g. /timesheets/week,
// /settings/users) so a screen can be bookmarked or referenced in a review.
// The server serves the same shell for these paths (`spa_page` in lib.rs);
// this router maps a path back to the tab/segment/panel and keeps the address
// bar in step. Keep the route table in sync with `spa_page`.

export let routerReady = false;

export const DEFAULT_ROUTE = '/timesheets/day';
export const TAB_ROUTES = {
  'tab-timesheet': {
    'ts-day': '/timesheets/day',
    'ts-week': '/timesheets/week',
    'ts-cal': '/timesheets/calendar',
  },
  'tab-customers': '/customers',
  'tab-projects': '/projects',
  'tab-tasks': '/tasks',
  'tab-invoices': '/invoices',
  'tab-expenses': '/expenses',
  'tab-submissions': '/approvals',
  'tab-reports': '/reports',
  'tab-settings': {
    'settings-tab-users': '/settings/users',
    'settings-tab-import': '/settings/import',
    'settings-tab-company': '/settings/company',
    'settings-tab-backup': '/settings/backup',
    'settings-tab-invoice-template': '/settings/invoice-template',
    'settings-tab-catalog': '/settings/catalog',
    'settings-tab-expenses': '/settings/expenses',
    'settings-tab-email': '/settings/email',
    'settings-tab-security': '/settings/security',
  },
};
export const KNOWN_ROUTES = new Set(
  Object.values(TAB_ROUTES).flatMap((routes) =>
    (typeof routes === 'string' ? [routes] : Object.values(routes))),
);

export function activeTabId(tablist) {
  const tab = document.querySelector(`${tablist} [role="tab"][aria-selected="true"]`);
  return tab ? tab.id : '';
}

// The canonical path for the view currently shown.
export function currentRoute() {
  const tab = activeTabId('#tabs');
  const routes = TAB_ROUTES[tab];
  if (typeof routes === 'string') return routes;
  if (tab === 'tab-timesheet') return routes[segActiveId] || DEFAULT_ROUTE;
  if (tab === 'tab-settings') return routes[activeTabId('#settings-tabs')] || '/settings/security';
  return DEFAULT_ROUTE;
}

// Reflect the current view in the address bar. Sub-navigation inside a
// section replaces the entry (so Back leaves the section, not each segment);
// moving to a different section pushes a new one.
export function syncRoute(forceReplace = false) {
  if (!routerReady) return;
  const path = currentRoute();
  if (window.location.pathname === path) return;
  const sameSection = window.location.pathname.split('/')[1] === path.split('/')[1];
  const replace = forceReplace || sameSection;
  history[replace ? 'replaceState' : 'pushState']({ path }, '', path);
}

// Canonical route for a pathname, or null when it names no known view.
export function normaliseRoute(pathname) {
  const p = (pathname || '/').split('?')[0].replace(/\/+$/, '').toLowerCase();
  if (p === '' || p === '/') return DEFAULT_ROUTE;
  if (p === '/timesheets') return '/timesheets/day';
  if (p === '/settings') return '/settings/security';
  return KNOWN_ROUTES.has(p) ? p : null;
}

// Select the view named by `route` (already canonical). Returns false when
// the view is unavailable (e.g. an admin-only panel for a member).
export function applyRoute(route) {
  const [, top, sub] = route.split('/');
  const tabFor = {
    timesheets: 'tab-timesheet',
    customers: 'tab-customers',
    projects: 'tab-projects',
    tasks: 'tab-tasks',
    invoices: 'tab-invoices',
    expenses: 'tab-expenses',
    approvals: 'tab-submissions',
    reports: 'tab-reports',
    settings: 'tab-settings',
  }[top];
  const tab = tabFor && $(tabFor);
  if (!tab || tab.hidden) return false;
  switchTab(tabFor);
  if (tabFor === 'tab-timesheet') {
    const seg = { day: 'ts-day', week: 'ts-week', calendar: 'ts-cal' }[sub || 'day'];
    activateSeg(seg || 'ts-day', false);
  } else if (tabFor === 'tab-settings') {
    const settingsId = {
      users: 'settings-tab-users',
      company: 'settings-tab-company',
      backup: 'settings-tab-backup',
      'invoice-template': 'settings-tab-invoice-template',
      catalog: 'settings-tab-catalog',
      expenses: 'settings-tab-expenses',
      email: 'settings-tab-email',
      import: 'settings-tab-import',
      security: 'settings-tab-security',
    }[sub || 'security'];
    const target = $(settingsId || 'settings-tab-security');
    if (target && !target.hidden) target.click();
  }
  return true;
}

// Applies the initial URL (or the default view) and wires Back/Forward.
export function initRouter() {
  routerReady = true;
  applyRoute(normaliseRoute(window.location.pathname) || DEFAULT_ROUTE);
  syncRoute(true); // canonicalise, e.g. '/' -> '/timesheets/day'
  window.addEventListener('popstate', () => {
    applyRoute(normaliseRoute(window.location.pathname) || DEFAULT_ROUTE);
  });
}
// ---------------------------------------------------------------- tabs ---

// Resolves the sibling a roving-tabindex tablist should move to for a key,
// honouring the list's declared orientation (#258): a vertical tablist
// navigates with Up/Down, a horizontal one with Left/Right. Per the WAI-ARIA
// tabs pattern the default orientation is horizontal when `aria-orientation`
// is absent. Home/End always jump to the ends; returns `null` for any key
// that does not navigate this list.
export function nextTabIndex(key, index, count, orientation) {
  const vertical = orientation === 'vertical';
  if (key === (vertical ? 'ArrowDown' : 'ArrowRight')) return (index + 1) % count;
  if (key === (vertical ? 'ArrowUp' : 'ArrowLeft')) return (index - 1 + count) % count;
  if (key === 'Home') return 0;
  if (key === 'End') return count - 1;
  return null;
}

export function initResponsiveNavigation() {
  const drawer = $('navigation-drawer');
  const sidebar = $('sidebar');
  const opener = $('navigation-open');
  if (!drawer || !sidebar || !opener || !window.matchMedia || drawer.dataset.navigationWired) return;
  drawer.dataset.navigationWired = '1';
  const narrow = window.matchMedia('(width < 1036px)');
  const content = document.querySelector('.content');
  const close = () => {
    if (drawer.open) drawer.close();
    opener.setAttribute('aria-expanded', 'false');
  };
  const sync = () => {
    const focused = document.activeElement;
    close();
    if (narrow.matches) {
      drawer.append(sidebar);
      if (sidebar.contains(focused)) opener.focus({ preventScroll: true });
    } else {
      content.before(sidebar);
      if (focused === opener || sidebar.contains(focused)) {
        sidebar.querySelector('[role="tab"][aria-selected="true"]')?.focus({ preventScroll: true });
      }
    }
  };
  opener.addEventListener('click', () => {
    if (!narrow.matches || drawer.open) return;
    opener.focus({ preventScroll: true });
    drawer.showModal();
    opener.setAttribute('aria-expanded', 'true');
  });
  $('navigation-close').addEventListener('click', close);
  drawer.addEventListener('close', () => opener.setAttribute('aria-expanded', String(drawer.open)));
  drawer.addEventListener('keydown', (event) => {
    if (event.key === 'Escape') {
      event.preventDefault();
      close();
    }
  });
  drawer.addEventListener('click', (event) => {
    const bounds = drawer.getBoundingClientRect();
    if (event.target === drawer && (event.clientX < bounds.left || event.clientX > bounds.right
      || event.clientY < bounds.top || event.clientY > bounds.bottom)) close();
  });
  sidebar.addEventListener('click', (event) => {
    if (!drawer.open) return;
    const action = event.target.closest('[role="tab"], #wizard-open, #logout');
    if (!action) return;
    close();
    if (action.matches('[role="tab"]')) {
      queueMicrotask(() => $('main').focus({ preventScroll: true }));
    }
  }, true);
  window.addEventListener('popstate', close);
  narrow.addEventListener('change', sync);
  sync();
}

export function initTabs() {
  initResponsiveNavigation();
  for (const region of document.querySelectorAll('.table-scroll, .invoice-table-scroll')) {
    const table = region.querySelector('table');
    region.tabIndex = 0;
    region.setAttribute('role', 'region');
    region.setAttribute('aria-label', table?.caption?.textContent.trim()
      || table?.getAttribute('aria-label') || 'Scrollable table');
  }
  const tabs = Array.from(document.querySelectorAll('#tabs [role="tab"]'));
  // A tab inside a collapsed disclosure is neither visible nor focusable, so the
  // roving focus must skip it: otherwise arrow keys move the user's focus onto
  // something they cannot see (#258). Every tab still keeps its ARIA state.
  const reachable = (tablist) => Array.from(tablist.querySelectorAll('[role="tab"]'))
    .filter((t) => !t.hidden && !t.closest('details:not([open])'));
  function activate(tab) {
    const tablist = tab.closest('[role="tablist"]');
    const siblings = Array.from(tablist.querySelectorAll('[role="tab"]'));
    // The rail shows where you are even when the tab is behind the disclosure
    // (a deep link, or a keyboard jump onto it).
    const group = tab.closest('details');
    if (group) group.open = true;
    siblings.forEach((t) => {
      const on = t === tab;
      t.setAttribute('aria-selected', String(on));
      t.tabIndex = on ? 0 : -1;
    });
    tabs.forEach((t) => {
      const panel = $(t.getAttribute('aria-controls'));
      if (panel) panel.hidden = t !== tab;
    });
    const title = $('page-title');
    if (title) title.textContent = tab.textContent.trim();
    // #365: the sidebar hides the whole Settings section without touching any
    // inner panel's `hidden`, so the preview's own observer never sees this
    // switch. Release the frame when leaving Settings, and re-arm it when the
    // section comes back (a still-loaded template renders itself again).
    if (tab.id === 'tab-settings') resumeTemplatePreview();
    else releaseTemplatePreview();
    focusTab(tab);
    syncRoute();
    // Re-pull the panel's data on show so it reflects changes made elsewhere
    // (e.g. an invoice issued via the API) without a full reload.
    refreshPanel(tab.id);
  }
  tabs.forEach((t) => {
    t.addEventListener('click', () => activate(t));
    t.addEventListener('keydown', (e) => {
      const tablist = t.closest('[role="tablist"]');
      const siblings = reachable(tablist);
      const i = siblings.indexOf(t);
      // #258: the sidebar is a *vertical* tablist (aria-orientation="vertical"),
      // so navigate along its declared axis (Up/Down) — not Left/Right.
      const j = nextTabIndex(e.key, i, siblings.length, tablist.getAttribute('aria-orientation'));
      if (j !== null) { e.preventDefault(); activate(siblings[j]); }
    });
  });
}

