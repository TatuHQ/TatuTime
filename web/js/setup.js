// setup.js — TatuTime GUI module: the admin startup wizard.
//
// First-run flow separate from the #111 customer/project wizard
// (wizard.js, #wizard-dialog): five steps that create the first administrator,
// record the organisation identity and the base currency, then re-read what the
// server actually persisted before entering the application. Every field name
// sent here is one the checked-in contract accepts — `BootstrapInput`
// (src/api/auth.rs), `OrgProfile`/`Address` (src/domain/customer.rs,
// openapi.json) and the whitelisted `base_currency` key (src/appconfig.rs).
//
// The dialog stays escapable: closing before the administrator exists falls
// back to the existing #auth-form "Create the first administrator" card, so a
// first-run instance can never be left with no way to create an admin.

import { api } from './api.js';
import { $, announce, clearFormError, showFormError } from './dom.js';
import { startApp } from './boot.js';
import { state } from './state.js';

export const AW_TITLES = [
    'Welcome to TatuTime',
    'Administrator',
    'Organisation',
    'Defaults',
    'Review',
];

// The wizard's state. `firstRun` marks the flow opened instead of the bare auth
// card; `created` records that the bootstrap call already succeeded, so closing
// afterwards enters the app instead of offering to create the admin again.
export const aw = {
    step: 1,
    firstRun: false,
    created: false,
    busy: false,
    opener: null,
    onCancel: null,
    onSignedIn: null,
    onExistingAdmin: null,
};

const awDialog = () => $('admin-wizard');

export function awOpen() {
    const dlg = awDialog();
    return !!dlg && (dlg.open === true || dlg.hasAttribute('open'));
}

export function awShow() {
    const dlg = awDialog();
    if (!dlg || awOpen()) return;
    if (typeof dlg.showModal === 'function') dlg.showModal();
    else dlg.setAttribute('open', '');
}

export function awHide() {
    const dlg = awDialog();
    if (!dlg || !awOpen()) return;
    if (typeof dlg.close === 'function') dlg.close();
    else dlg.removeAttribute('open');
}

// Focus the first meaningful control of the current step. Buttons on the
// welcome/review steps, the first field everywhere else.
function awFocusTarget(step) {
    return {
        1: 'aw-next',
        2: 'aw-name',
        3: 'aw-org-name',
        4: 'aw-currency',
        5: 'aw-next',
    }[step] || 'aw-next';
}

function awSetStatus(text) {
    $('aw-status').textContent = text;
}

function awReject(text, fieldId) {
    const box = $('aw-error');
    box.textContent = text;
    box.hidden = false;
    const field = fieldId ? $(fieldId) : null;
    if (field) {
        field.setAttribute('aria-invalid', 'true');
        field.setAttribute('aria-describedby', box.id);
        field.dataset.errorMessage = box.id;
        field.focus();
    }
}

export function awRender() {
    for (let n = 1; n <= 5; n += 1) $(`aw-step-${n}`).hidden = n !== aw.step;
    $('aw-title').textContent = AW_TITLES[aw.step - 1];
    awSetStatus(`Step ${aw.step} of 5: ${AW_TITLES[aw.step - 1]}`);
    $('aw-back').hidden = aw.step === 1;
    $('aw-skip').hidden = !(aw.step === 3 || aw.step === 4);
    $('aw-next').textContent = {
        1: 'Get started',
        2: 'Create administrator',
        3: 'Save organisation',
        4: 'Save defaults',
        5: 'Enter the application',
    }[aw.step];
    clearFormError($('aw-error'));
    const target = $(awFocusTarget(aw.step));
    if (target) target.focus();
}

function awGoTo(step) {
    aw.step = step;
    awRender();
    if (step === 4) awLoadCurrency();
    if (step === 5) awLoadReview();
}

export function openAdminWizard(opts = {}) {
    aw.firstRun = Boolean(opts.firstRun);
    aw.created = false;
    aw.busy = false;
    aw.opener = opts.opener || document.activeElement;
    aw.onCancel = opts.onCancel || null;
    aw.onSignedIn = opts.onSignedIn || null;
    aw.onExistingAdmin = opts.onExistingAdmin || null;
    awShow();
    awGoTo(opts.step || 1);
}

// Closing is always allowed. Before the admin exists it restores the bare
// create-admin card (the first-run path); after a successful bootstrap it enters
// the app with whatever was saved, so closing never strands the instance.
export function closeAdminWizard() {
    if (!awOpen()) return;
    const wasFirstRun = aw.firstRun;
    const created = aw.created;
    const opener = aw.opener;
    awHide();
    aw.opener = null;
    aw.busy = false;
    if (wasFirstRun && !created) {
        aw.onCancel?.();
    } else if (wasFirstRun && created) {
        Promise.resolve(startApp()).catch((err) => announce(`Failed to start: ${err.message}`));
    } else if (opener && opener.isConnected && typeof opener.focus === 'function') {
        opener.focus();
    }
}

function awValidEmail(value) {
    return /^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(value);
}

async function awBootstrap() {
    const name = $('aw-name').value.trim();
    const email = $('aw-email').value.trim();
    const password = $('aw-password').value;
    const confirm = $('aw-confirm').value;
    if (!name) return awReject('name: required, at most 120 characters', 'aw-name');
    if (!email || !awValidEmail(email)) return awReject('email: invalid email', 'aw-email');
    if ([...password].length < 8) {
        return awReject('password: must be at least 8 characters', 'aw-password');
    }
    if ([...password].length > 200) return awReject('password: at most 200 characters', 'aw-password');
    if (password !== confirm) return awReject('confirm: passwords do not match', 'aw-confirm');
    aw.busy = true;
    try {
        // 201 on success; the server rejects a second bootstrap with 403 initialised.
        const me = await api.post('/auth/bootstrap', { name, email, password });
        aw.created = true;
        aw.onSignedIn?.(me);
        aw.busy = false;
        awGoTo(3);
        awSetStatus('Administrator created. Step 3 of 5: Organisation');
    } catch (err) {
        aw.busy = false;
        if (err.status === 403 && err.payload?.error?.code === 'initialised') {
            // Someone else created the admin meanwhile: sign in instead of retrying.
            awHide();
            aw.opener = null;
            aw.onExistingAdmin?.();
            announce('An administrator already exists — sign in.');
            return;
        }
        showFormError($('aw-error'), err);
    }
}

async function awSaveOrg() {
    const name = $('aw-org-name').value.trim();
    const legalId = $('aw-org-legal').value.trim();
    const street = $('aw-org-street').value.trim();
    const city = $('aw-org-city').value.trim();
    const postal = $('aw-org-postal').value.trim();
    const country = $('aw-org-country').value.trim();
    const accent = $('aw-org-accent').value.trim();
    if (!name) return awReject('name: required, at most 120 characters', 'aw-org-name');
    const hasAddress = Boolean(street || city || postal || country);
    if (hasAddress && country.length < 2) {
        return awReject('address.country: required when addressing an invoice', 'aw-org-country');
    }
    if (accent && !/^#[0-9a-fA-F]{6}$/.test(accent)) {
        return awReject('accent: expected a #rrggbb hex color', 'aw-org-accent');
    }
    // Exactly the OrgProfile fields that exist; empty optional ones are omitted.
    const body = { name };
    if (legalId) body.legal_id = legalId;
    if (hasAddress) body.address = { street, city, postal_code: postal, country };
    if (accent) body.accent = accent;
    aw.busy = true;
    try {
        await api.put('/admin/org', body);
        aw.busy = false;
        awGoTo(4);
    } catch (err) {
        aw.busy = false;
        showFormError($('aw-error'), err);
    }
}

async function awLoadCurrency() {
    try {
        const data = await api.get('/admin/config');
        const entry = (data.config || []).find((c) => c.key === 'base_currency');
        const current = $('aw-currency').value;
        if (!current && entry && typeof entry.value === 'string') {
            $('aw-currency').value = entry.value;
        }
    } catch {
        /* the default is optional; the step still saves or skips */
    }
}

async function awSaveDefaults() {
    const code = $('aw-currency').value.trim().toUpperCase();
    // Empty means "skip": no request, exactly like the "Set this up later" action.
    if (!code) {
        awGoTo(5);
        return;
    }
    if (!/^[A-Z]{3}$/.test(code)) {
        return awReject('base_currency: must be exactly three ASCII letters (ISO 4217)', 'aw-currency');
    }
    aw.busy = true;
    try {
        // The whitelisted key only (src/appconfig.rs: base_currency, Kind::Currency).
        await api.put('/admin/config', { base_currency: code });
        aw.busy = false;
        awGoTo(5);
    } catch (err) {
        aw.busy = false;
        showFormError($('aw-error'), err);
    }
}

async function awLoadReview() {
    $('aw-review-name').textContent = '';
    $('aw-review-email').textContent = '';
    $('aw-review-org').textContent = 'Loading…';
    try {
        const me = await api.get('/auth/me');
        $('aw-review-name').textContent = me.name || me.email || '';
        $('aw-review-email').textContent = me.email || '';
    } catch {
        $('aw-review-name').textContent = 'Unavailable';
    }
    try {
        const org = await api.get('/admin/org');
        const name = String(org.name || '').trim();
        $('aw-review-org').textContent = name || 'Not set';
    } catch {
        $('aw-review-org').textContent = 'Unavailable';
    }
}

export async function awNext() {
    if (aw.busy) return;
    clearFormError($('aw-error'));
    if (aw.step === 1) return awGoTo(2);
    if (aw.step === 2) return awBootstrap();
    if (aw.step === 3) return awSaveOrg();
    if (aw.step === 4) return awSaveDefaults();
    return awFinish();
}

export function awBack() {
    if (aw.busy || aw.step <= 1) return;
    awGoTo(aw.step - 1);
}

// "Set this up later" on the organisation/defaults steps: advance without
// issuing any request, so skipping never writes a half-filled document.
export function awSkip() {
    if (aw.busy) return;
    if (aw.step !== 3 && aw.step !== 4) return;
    awGoTo(aw.step + 1);
}

async function awFinish() {
    aw.busy = true;
    const firstRun = aw.firstRun;
    awHide();
    aw.opener = null;
    aw.busy = false;
    // A re-entry from the dashboard prompt runs over an app that is already
    // booted: re-running startApp() would wire every listener twice. Only the
    // first-run flow needs to enter the application.
    if (!firstRun) {
        announce('Setup complete — organisation settings saved.');
        return;
    }
    try {
        await startApp();
        announce('Setup complete. Welcome to TatuTime.');
    } catch (err) {
        announce(`Failed to start: ${err.message}`);
    }
}

// Offer the organisation half of the wizard again to a signed-in administrator
// whose org name is still empty. A dismissible dashboard prompt, never an
// auto-opened modal over the running app.
export async function offerOrgSetup() {
    const prompt = $('aw-prompt');
    if (!prompt) return;
    if (!state.isAdmin) {
        prompt.hidden = true;
        return;
    }
    try {
        const org = await api.get('/admin/org');
        const empty = !String((org && org.name) || '').trim();
        prompt.hidden = !empty;
    } catch {
        prompt.hidden = true;
    }
}

export function initAdminWizard() {
    const dlg = awDialog();
    if (!dlg || dlg.dataset.awWired) return;
    dlg.dataset.awWired = '1';
    $('aw-next').addEventListener('click', () => awNext());
    $('aw-back').addEventListener('click', () => awBack());
    $('aw-skip').addEventListener('click', () => awSkip());
    $('aw-close').addEventListener('click', () => closeAdminWizard());
    $('aw-form').addEventListener('submit', (e) => {
        e.preventDefault();
        awNext();
    });
    // Enter submits the step; Escape closes (native `cancel` plus the explicit
    // handler, so hosts without dialog keyboard behaviour still work).
    dlg.addEventListener('keydown', (e) => {
        if (e.key === 'Escape') {
            e.preventDefault();
            closeAdminWizard();
        } else if (e.key === 'Enter' && e.target.tagName !== 'BUTTON' && e.target.tagName !== 'TEXTAREA') {
            e.preventDefault();
            awNext();
        }
    });
    dlg.addEventListener('cancel', (e) => {
        e.preventDefault();
        closeAdminWizard();
    });
    $('aw-prompt-open').addEventListener('click', () => {
        $('aw-prompt').hidden = true;
        openAdminWizard({ step: 3, opener: $('aw-prompt-open') });
    });
    $('aw-prompt-dismiss').addEventListener('click', () => {
        $('aw-prompt').hidden = true;
    });
}
