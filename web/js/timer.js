// timer.js — TatuTime GUI module (ES-module split of web/app.js, #264).
// Moved from the original monolithic app.js; behaviour is unchanged.


import { api } from './api.js';
import { $, announce } from './dom.js';
import { refreshDay } from './timesheet.js';

// ------------------------------------------------------------------- timer --

export let timerBase = 0;
export let timerInterval = null;

export function fmtHMS(total) {
  const h = String(Math.floor(total / 3600)).padStart(2, '0');
  const m = String(Math.floor((total % 3600) / 60)).padStart(2, '0');
  const s = String(total % 60).padStart(2, '0');
  return `${h}:${m}:${s}`;
}

export function stopTimerTick() {
  if (timerInterval) {
    clearInterval(timerInterval);
    timerInterval = null;
  }
}

export function startTimerTick(elapsedSeconds) {
  timerBase = elapsedSeconds;
  $('timer-display').textContent = fmtHMS(timerBase);
  stopTimerTick();
  timerInterval = setInterval(() => {
    timerBase += 1;
    $('timer-display').textContent = fmtHMS(timerBase);
  }, 1000);
}

export async function refreshTimer() {
  const data = await api.get('/timer');
  const running = !!data;
  $('timer-open').hidden = running;
  if (running) {
    $('timerbar').hidden = true;
    $('timer-open').setAttribute('aria-expanded', 'false');
  }
  ['timer-display', 'timer-stop', 'timer-discard'].forEach((id) => {
    $(id).hidden = !running;
  });
  if (running) {
    startTimerTick(data.elapsed_seconds);
  } else {
    stopTimerTick();
  }
}

export async function startTimer() {
  const customerId = $('timer-customer').value;
  const projectCode = $('timer-project').value;
  if (!customerId || !projectCode) {
    announce('Choose a customer and project first.');
    return;
  }
  try {
    await api.post('/timer', { customer_id: customerId, project_code: projectCode });
    await refreshTimer();
    announce('Timer started.');
  } catch (err) {
    announce(err.message);
  }
}

export async function stopTimer() {
  try {
    await api.post('/timer/stop');
    await refreshTimer();
    await refreshDay();
    announce('Timer stopped and logged.');
  } catch (err) {
    announce(err.message);
  }
}

export async function discardTimer() {
  try {
    await api.del('/timer');
    await refreshTimer();
    announce('Timer discarded.');
  } catch (err) {
    announce(err.message);
  }
}

