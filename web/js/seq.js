// seq.js — last-call-wins async render guard factory (#264).
//
// Five views hand-rolled the same stale-response guard: bump a module-level
// counter, then bail if a newer render bumped it again. begin() hands out a
// ticket and a predicate that stays true only while that ticket is newest
// (#188/#219 semantics, unchanged).
export function createSeqGuard() {
  let latest = 0;
  return function begin() {
    const seq = ++latest;
    return function isCurrent() {
      return seq === latest;
    };
  };
}
