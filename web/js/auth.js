// auth.js — TucanoTime GUI module (ES-module split of web/app.js, #264).
// Moved from the original monolithic app.js; behaviour is unchanged.


import { api } from './api.js';
import { startApp } from './boot.js';
import { $, announce, clearFormError, el, initDialog, initDialogFocusReturn, showFormError } from './dom.js';
import { initAdminWizard, offerOrgSetup, openAdminWizard } from './setup.js';
import { state } from './state.js';

// ----------------------------------------------------------------- auth ---

export let authMode = 'login';

export function showAuth(mode) {
  authMode = mode;
  $('auth-overlay').hidden = false;
  if (mode === 'setup') {
    $('auth-title').textContent = 'Create the first administrator';
    $('auth-sub').textContent = 'No accounts exist yet — set up the admin login.';
    $('auth-name-field').hidden = false;
    $('auth-submit').textContent = 'Create admin';
    $('auth-password').setAttribute('autocomplete', 'new-password');
  } else {
    $('auth-title').textContent = 'Sign in';
    $('auth-sub').textContent = '';
    $('auth-name-field').hidden = true;
    $('auth-submit').textContent = 'Sign in';
    $('auth-password').setAttribute('autocomplete', 'current-password');
  }
  $('auth-email').focus();
}

export function showAccount(me) {
  $('auth-overlay').hidden = true;
  $('account').hidden = false;
  $('who').textContent = me.name || me.email;
  const admin = me.role === 'admin';
  state.isAdmin = admin; // #188: boot must skip admin-tier panels for members
  document.querySelectorAll('#settings-tabs [role="tab"]').forEach((tab) => {
    tab.hidden = !admin && tab.id !== 'settings-tab-expenses';
  });
  if (!admin) {
    document.querySelectorAll('#settings-tabs [role="tab"]').forEach((tab) => {
      const active = tab.id === 'settings-tab-expenses';
      tab.setAttribute('aria-selected', String(active));
      tab.tabIndex = active ? 0 : -1;
      $(tab.getAttribute('aria-controls')).hidden = !active;
    });
  }
}

export async function initAuth() {
  const st = await api.get('/auth/status');
  if (!st.initialised) {
    // The admin startup wizard is the primary first-run surface. Closing it
    // falls back to the existing create-admin card (#auth-form), so the
    // instance can never be left with no way to create an administrator.
    openAdminWizard({
      firstRun: true,
      onCancel: () => showAuth('setup'),
      onSignedIn: (me) => showAccount(me),
      onExistingAdmin: () => showAuth('login'),
    });
    return false;
  }
  try {
    const me = await api.get('/auth/me');
    showAccount(me);
    return true;
  } catch {
    showAuth('login');
    return false;
  }
}

export async function onAuthSubmit(evt) {
  evt.preventDefault();
  clearFormError($('auth-error'));
  const email = $('auth-email').value.trim();
  const password = $('auth-password').value;
  try {
    const me =
      authMode === 'setup'
        ? await api.post('/auth/bootstrap', { name: $('auth-name').value.trim(), email, password })
        : await api.post('/auth/login', { email, password });
    showAccount(me);
    await startApp();
  } catch (err) {
    showFormError($('auth-error'), err);
  }
}

export async function logout() {
  try {
    await api.post('/auth/logout');
  } catch {
    /* best effort */
  }
  window.location.reload();
}

document.addEventListener('DOMContentLoaded', async () => {
  initDialog(); // one <dialog> for confirm/prompt/alert (#102)
  initDialogFocusReturn(); // #232 inc 4: every popup returns focus to its opener
  initAdminWizard(); // first-run administrator + organisation flow
  $('auth-form').addEventListener('submit', onAuthSubmit);
  $('logout').addEventListener('click', logout);
  loadSsoProviders(); // advertise configured identity providers (#32)
  try {
    if (await initAuth()) {
      await startApp();
      // A dismissible dashboard prompt only — never an auto-opened modal. Not
      // awaited: it is one optional GET and must not gate the boot handler.
      offerOrgSetup();
    }
  } catch (err) {
    announce(`Failed to start: ${err.message}`);
  }
});

// Shows "Sign in with …" buttons for each configured SSO provider. The IdP
// redirect/POST bindings land with the production adapters; until then the
// buttons explain the ACS endpoint (honest Beta behaviour, nothing silently
// fails).
export async function loadSsoProviders() {
  try {
    const st = await api.get('/auth/sso/providers');
    const providers = st.providers || [];
    if (providers.length === 0) return;
    $('sso-box').hidden = false;
    const box = $('sso-buttons');
    box.textContent = '';
    for (const name of providers) {
      box.appendChild(
        el('button', {
          type: 'button',
          cls: 'p-button',
          text: `Sign in with ${name}`,
          on: {
            click: () => {
              announce(
                `Point your ${name} IdP at POST /api/auth/sso/assertion (provider "${name}"). ` +
                'Local sign-in stays available.',
              );
            },
          },
        }),
      );
    }
  } catch {
    /* providers are optional */
  }
}

