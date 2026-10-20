import { el } from './dom.js';

let activeSelectMenu = null;
let selectMenuId = 0;
let selectObserver = null;

export function closeSelectMenus() { activeSelectMenu?.(); }

export function enhanceSelect(select, caption = '') {
  if (!select || select.multiple || select.size > 1) return;
  const name = caption || select.getAttribute('aria-label') || select.labels?.[0]?.textContent.trim() || 'Options';
  if (select.__filterSync) {
    if (caption) select.__selectCaption = name;
    select.__filterSync(); return;
  }
  select.__selectCaption = name;
  const id = select.id || (select.id = `select-${++selectMenuId}`);
  const text = el('span', { cls: 'invoice-filter-label' });
  const button = el('button', { type: 'button', cls: 'p-button invoice-filter-trigger select-trigger',
    data: { icon: 'chevron-down' }, attrs: { id: `${id}-button`, 'aria-haspopup': 'listbox',
      'aria-controls': `${id}-menu`, 'aria-expanded': 'false' } }, [text]);
  const menu = el('div', { cls: 'invoice-filter-menu', attrs: {
    id: `${id}-menu`, role: 'listbox', 'aria-label': name, popover: 'auto', hidden: '',
  } });
  if (typeof menu.showPopover !== 'function') menu.removeAttribute('popover');
  let validation = null;
  let typed = '';
  let typeTimer;
  const close = (restore = false) => {
    if (menu.matches(':popover-open')) menu.hidePopover();
    menu.hidden = true;
    button.setAttribute('aria-expanded', 'false');
    document.removeEventListener('pointerdown', outside);
    window.removeEventListener('resize', position);
    window.removeEventListener('scroll', position, true);
    if (activeSelectMenu === close) activeSelectMenu = null;
    if (restore && button.isConnected) button.focus({ preventScroll: true });
  };
  const position = () => {
    if (menu.hidden) return;
    if (!button.isConnected || (button.checkVisibility && !button.checkVisibility())) { close(); return; }
    const anchor = button.getBoundingClientRect();
    if (anchor.bottom < 0 || anchor.top > innerHeight) { close(); return; }
    const width = Math.min(Math.max(anchor.width, 192), innerWidth - 16);
    menu.style.width = `${width}px`;
    menu.style.left = `${Math.max(8, Math.min(anchor.left, innerWidth - width - 8))}px`;
    const below = innerHeight - anchor.bottom - 8;
    const above = anchor.top - 8;
    const upwards = below < Math.min(menu.scrollHeight, 180) && above > below;
    menu.style.maxHeight = `${Math.max(40, Math.min(320, upwards ? above : below))}px`;
    menu.style.top = `${upwards ? anchor.top - menu.getBoundingClientRect().height - 4 : anchor.bottom + 4}px`;
  };
  const sync = () => {
    const selected = select.selectedOptions[0];
    const placeholder = id === 'inv-f-customer' || select.dataset.catalog === '1';
    text.textContent = !select.value && placeholder ? select.__selectCaption : selected?.textContent || select.__selectCaption;
    button.title = `${select.__selectCaption}: ${selected?.textContent || ''}`;
    button.setAttribute('aria-label', button.title);
    button.disabled = select.disabled;
    button.hidden = select.hidden || (select.dataset.catalog === '1' && select.options.length < 2);
    button.setAttribute('aria-required', String(select.required));
    for (const attribute of ['aria-invalid', 'aria-describedby']) {
      if (select.hasAttribute(attribute)) button.setAttribute(attribute, select.getAttribute(attribute));
      else button.removeAttribute(attribute);
    }
    if (button.hidden || button.disabled) close();
  };
  const open = () => {
    if (button.disabled || button.hidden) return;
    closeSelectMenus();
    menu.replaceChildren(...[...select.options].map((option, index) => {
      const item = el('button', { type: 'button', cls: 'p-button invoice-filter-option', text: option.textContent,
        attrs: { role: 'option', tabindex: '-1', 'aria-selected': String(option.selected), id: `${id}-option-${index}` },
        on: { click: () => {
          if (option.disabled) return;
          select.value = option.value;
          validation?.remove(); validation = null;
          select.dispatchEvent(new Event('input', { bubbles: true }));
          select.dispatchEvent(new Event('change', { bubbles: true }));
          sync(); close(true);
        } } });
      item.disabled = option.disabled || Boolean(option.closest('optgroup')?.disabled);
      return item;
    }));
    menu.hidden = false;
    menu.showPopover?.();
    activeSelectMenu = close;
    button.setAttribute('aria-expanded', 'true');
    document.addEventListener('pointerdown', outside);
    window.addEventListener('resize', position);
    window.addEventListener('scroll', position, true);
    position();
    (menu.querySelector('[aria-selected="true"]:not(:disabled)') || menu.querySelector('button:not(:disabled)'))?.focus({ preventScroll: true });
  };
  const outside = event => { if (!menu.contains(event.target) && !button.contains(event.target)) close(); };
  button.addEventListener('click', () => menu.hidden ? open() : close());
  button.addEventListener('keydown', event => {
    if (['ArrowDown', 'ArrowUp'].includes(event.key)) { event.preventDefault(); open(); }
  });
  menu.addEventListener('keydown', event => {
    if (event.key === 'Escape') { event.preventDefault(); event.stopPropagation(); close(true); return; }
    if (event.key === 'Tab') { close(true); return; }
    const options = [...menu.querySelectorAll('[role="option"]:not(:disabled)')];
    const current = options.indexOf(document.activeElement);
    let next = event.key === 'Home' ? 0 : event.key === 'End' ? options.length - 1
      : event.key === 'ArrowDown' ? (current + 1) % options.length
        : event.key === 'ArrowUp' ? (current - 1 + options.length) % options.length : null;
    if (event.key.length === 1 && !event.ctrlKey && !event.metaKey) {
      clearTimeout(typeTimer); typed += event.key.toLowerCase();
      typeTimer = setTimeout(() => { typed = ''; }, 700);
      next = options.findIndex(option => option.textContent.trim().toLowerCase().startsWith(typed));
    }
    if (next !== null && next >= 0) { event.preventDefault(); options[next]?.focus(); }
  });
  menu.addEventListener('toggle', event => { if (event.newState === 'closed' && !menu.matches(':popover-open')) close(); });
  select.classList.add('invoice-filter-native');
  select.after(button, menu);
  select.addEventListener('change', sync);
  select.addEventListener('invalid', event => {
    event.preventDefault(); button.setAttribute('aria-invalid', 'true'); button.focus();
    if (!validation) { validation = el('span', { cls: 'select-validation', attrs: { role: 'alert' } }); button.after(validation); }
    validation.textContent = select.validationMessage;
  });
  select.form?.addEventListener('reset', () => queueMicrotask(() => { validation?.remove(); validation = null; sync(); }));
  for (const property of ['value', 'selectedIndex']) {
    const descriptor = Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, property);
    Object.defineProperty(select, property, { configurable: true,
      get() { return descriptor.get.call(this); },
      set(value) { descriptor.set.call(this, value); sync(); },
    });
  }
  new MutationObserver(sync).observe(select, { childList: true, subtree: true, characterData: true,
    attributes: true, attributeFilter: ['disabled', 'hidden', 'required', 'selected', 'aria-invalid', 'aria-describedby'] });
  select.__filterSync = sync;
  select.__filterButton = button;
  select.__closeMenu = close;
  for (const label of select.labels || []) label.addEventListener('click', event => { event.preventDefault(); button.focus(); });
  sync();
}

export function initSelectMenus() {
  document.querySelectorAll('select').forEach(select => enhanceSelect(select));
  if (selectObserver) return;
  selectObserver = new MutationObserver(records => {
    for (const record of records) {
      for (const node of record.addedNodes) {
        if (node.nodeType !== 1) continue;
        if (node.matches('select')) enhanceSelect(node);
        node.querySelectorAll('select').forEach(select => enhanceSelect(select));
      }
    }
  });
  selectObserver.observe(document.body, { childList: true, subtree: true });
}