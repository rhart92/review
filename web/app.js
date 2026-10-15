'use strict';
const $ = selector => document.querySelector(selector);
const el = (tag, className, text) => {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
};
const button = (label, action, className) => {
  const node = el('button', className, label);
  node.type = 'button'; node.onclick = action;
  return node;
};
let review, selected, comments = [], storageKey, composer, busy = false, queued = false;
let storageError = '', storageLoadFailed = false, lastSync = '', requestVersion = 0;
const initialQuery = new URL(location.href).searchParams;
let sourcePr = initialQuery.get('pr'), repoPath = initialQuery.get('repo'), baseRef = initialQuery.get('base');
let serverConfig = null, loadedBase = null, lastRefreshError = '';
let repoDirectory = null, repoBrowseVersion = 0, repoBrowsing = false, pendingRepoSelection = null;
let dragSelection = null, dragFrame = null;
const views = new Map();
const sections = new Map();
const status = (text, error = false) => { $('#status').textContent = text; $('#status').className = error ? 'error' : ''; };
const endLine = comment => comment.endLine ?? comment.line;
const anchorLabel = comment => `${comment.path}:${comment.line}${endLine(comment) !== comment.line ? `-${endLine(comment)}` : ''}${comment.side === 'old' ? ' (old)' : ''}`;
const lineKey = line => `${line.kind}:${line.old}:${line.new}`;
function selectionQuery(repo = repoPath, base = baseRef, pr = sourcePr) {
  const params = new URLSearchParams();
  if (repo) params.set('repo', repo);
  if (base) params.set('base', base);
  if (pr !== null) params.set('pr', pr);
  return params.toString();
}
const sourceQuery = () => selectionQuery();
function writeSelectionUrl() {
  const url = new URL(location.href);
  for (const [key, value] of [['repo', repoPath], ['base', baseRef], ['pr', sourcePr]]) {
    if (value !== null && value !== '') url.searchParams.set(key, value);
    else url.searchParams.delete(key);
  }
  history.replaceState(null, '', url);
  $('#pr-number').value = sourcePr && sourcePr !== '0' ? sourcePr : '';
}
async function api(path) {
  const response = await fetch(path, { cache: 'no-store' });
  const data = await response.json();
  if (!response.ok) throw new Error(data.error || `Request failed (${response.status})`);
  return data;
}
function updateCount() {
  $('#comments-button').textContent = `Comments (${comments.length})`;
  $('#copy').disabled = comments.length === 0;
  $('#clear-comments').disabled = comments.length === 0 && !composer;
}
function loadComments() {
  const nextKey = `local-review:${JSON.stringify(review.source === 'pr' ? [review.repo, review.base, review.pr_url] : [review.repo, review.base])}`;
  if (nextKey === storageKey) return;
  storageKey = nextKey; comments = []; storageError = ''; storageLoadFailed = false;
  try {
    const saved = JSON.parse(localStorage.getItem(storageKey) || '[]');
    if (!Array.isArray(saved) || !saved.every(c => typeof c.id === 'string' && typeof c.path === 'string' && ['old', 'new'].includes(c.side) && Number.isInteger(c.line) && c.line > 0 && (c.endLine === undefined || Number.isInteger(c.endLine) && c.endLine >= c.line) && typeof c.body === 'string' && typeof c.lineText === 'string')) throw new Error('Invalid saved comments');
    comments = saved;
  } catch (error) {
    storageLoadFailed = true;
    storageError = `Cannot load browser comments: ${error.message}. Existing storage has not been overwritten; new comments are in memory only. Copy feedback before closing.`;
  }
  updateCount();
}
function persist() {
  updateCount();
  if (storageLoadFailed) { status(storageError, true); return false; }
  try { localStorage.setItem(storageKey, JSON.stringify(comments)); storageError = ''; return true; }
  catch (error) { storageError = `Comments are in memory only; browser storage failed: ${error.message}. Copy feedback before closing.`; status(storageError, true); return false; }
}
function syncStatus() {
  if (storageError) return status(storageError, true);
  const source = review.source === 'pr' ? `${review.base} → pushed PR head` : `${review.base} → working tree · committed + staged + unstaged + untracked`;
  status(`${source} · ${lastSync}${composer ? ' · finish your comment to refresh' : ''}`);
}
function isTestFile(path) {
  return /(^|\/)(__tests?__|tests?|__test__|__specs__|specs?|e2e|__snapshots__)(\/|$)/i.test(path)
    || /(?:^|[./_-])(?:test|spec|tests|specs)(?:[._-]|$)/i.test(path.split('/').at(-1))
    || /(?:^|\/)test_[^/]+$|_test\.[^/]+$|\.snap$/i.test(path);
}
function visibleFiles() {
  const filter = $('#filter').value.toLowerCase();
  return review.files.filter(file => file.path.toLowerCase().includes(filter) && (!$('#hide-tests').checked || !isTestFile(file.path)));
}
function diffCounts(view) {
  if (view.counts) return view.counts;
  let additions = 0, deletions = 0;
  for (const group of view.parsed.groups) {
    for (const line of group) {
      if (line.kind === 'add') additions++;
      else if (line.kind === 'del') deletions++;
    }
  }
  return view.counts = { additions, deletions };
}
function countBadge(view, className) {
  const counts = diffCounts(view), badge = el('span', className);
  badge.append(el('span', 'count-add', `+${counts.additions.toLocaleString()}`), el('span', 'count-delete', `−${counts.deletions.toLocaleString()}`));
  badge.setAttribute('aria-label', `${counts.additions} lines added, ${counts.deletions} lines removed`);
  return badge;
}
function updateSummary(files) {
  let additions = 0, deletions = 0;
  for (const file of files) {
    const counts = diffCounts(views.get(file.path));
    additions += counts.additions; deletions += counts.deletions;
  }
  $('#summary-files').textContent = `${files.length} file${files.length === 1 ? '' : 's'}`;
  $('#summary-additions').textContent = `+${additions.toLocaleString()}`;
  $('#summary-additions').setAttribute('aria-label', `${additions} lines added`);
  $('#summary-deletions').textContent = `−${deletions.toLocaleString()}`;
  $('#summary-deletions').setAttribute('aria-label', `${deletions} lines removed`);
  $('#diff-summary').title = 'Changed lines in visible files; unchanged context is excluded.';
}
async function copyText(text, label) {
  try { await navigator.clipboard.writeText(text); return true; }
  catch {
    const dialog = $('#export-dialog');
    dialog.querySelector('h2').textContent = label;
    dialog.classList.toggle('path-copy', label === 'File path');
    $('#export-text').value = text;
    $('#export-text').setAttribute('aria-label', `${label} to copy`);
    dialog.showModal(); $('#export-text').select();
    return false;
  }
}
async function copyPath(path) {
  if (await copyText(path, 'File path')) status(`Copied path: ${path}`);
}
function renderFiles() {
  const files = visibleFiles(); $('#files').replaceChildren();
  for (const file of files) {
    const node = button('', () => { jumpToFile(file.path); copyPath(file.path); }, file.path === selected ? 'active' : '');
    node.append(el('span', `badge ${file.status}`, file.status), el('span', 'sidebar-path', file.path), countBadge(views.get(file.path), 'sidebar-counts'));
    node.title = `Jump to file and copy path: ${file.path}`; $('#files').append(node);
  }
  $('#file-count').textContent = files.length === review.files.length ? files.length : `${files.length}/${review.files.length}`;
  updateSummary(files);
}
function jumpToFile(path) {
  selected = path; renderFiles();
  sections.get(path)?.scrollIntoView({ block: 'start' });
}
async function refresh() {
  if (!serverConfig) return false;
  if (composer || dragSelection) { status('Finish selecting or editing your comment before refreshing the whole diff.', true); return; }
  if (!repoPath && !serverConfig?.repo) { status('Choose a repository to begin.'); return false; }
  if (busy) { queued = true; return; }
  busy = true; $('#refresh').disabled = true; lastRefreshError = ''; updateRepoControls();
  const version = requestVersion, requestedBase = baseRef ?? serverConfig.base;
  status('Loading the whole diff…');
  try {
    const next = await api(`api/review${sourceQuery() ? `?${sourceQuery()}` : ''}`);
    if (version !== requestVersion || composer || dragSelection || selectedCodeRange()) return;
    const changed = !review || JSON.stringify(next) !== JSON.stringify(review);
    if (review && review.repo !== next.repo) { $('#filter').value = ''; selected = undefined; }
    if (review && (review.repo !== next.repo || review.base !== next.base || review.source !== next.source || review.pr_url !== next.pr_url)) views.clear();
    review = next; repoPath = next.repo; baseRef = requestedBase; loadedBase = requestedBase; loadComments();
    $('#repo').textContent = review.repo;
    updateRepoLabel(); writeSelectionUrl();
    $('#branch').textContent = review.source === 'pr' ? review.title || 'Pull request' : `${review.branch} / ${review.base}`;
    $('#pr-link').hidden = !review.pr_url;
    if (review.pr_url) { $('#pr-link').href = review.pr_url; $('#pr-link').textContent = 'Open PR'; }
    for (const file of review.files) {
      const old = views.get(file.path);
      if (!old || old.patch !== file.patch || old.baseOid !== review.base_oid) views.set(file.path, { patch: file.patch, baseOid: review.base_oid, parsed: parsePatch(file.patch) });
    }
    for (const path of views.keys()) if (!review.files.some(file => file.path === path)) views.delete(path);
    const scroll = $('#main').scrollTop;
    renderFiles();
    if (changed || !$('#main .review-file')) { renderDiff(); $('#main').scrollTop = scroll; }
    lastSync = `updated ${new Date().toLocaleTimeString()}`; lastPoll = Date.now(); syncStatus();
    return true;
  } catch (error) { lastRefreshError = error.message; status(`${error.message} · Refresh all to retry.`, true); return false; }
  finally {
    busy = false; $('#refresh').disabled = false; updateRepoControls();
    if (queued) { queued = false; queueMicrotask(refresh); }
  }
}
function parsePatch(patch) {
  const groups = [], notes = []; let old = 0, current = 0, group;
  const rows = patch.split('\n'); if (rows.at(-1) === '') rows.pop();
  for (const row of rows) {
    if (/^(?:new file mode|deleted file mode|old mode|new mode) [0-7]+$/.test(row)) continue;
    const match = /^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@/.exec(row);
    if (match) { old = Number(match[1]); current = Number(match[2]); group = []; groups.push(group); continue; }
    if (group && /^[ +\-]/.test(row)) {
      const kind = row[0] === '+' ? 'add' : row[0] === '-' ? 'del' : 'context';
      group.push({ kind, text: row.slice(1), old: kind === 'add' ? null : old++, new: kind === 'del' ? null : current++ });
    } else if (group || /Binary files|GIT binary patch|Submodule|similarity index/.test(row)) notes.push(row);
  }
  return { groups, notes };
}
function groupsFor(view) {
  return view.full ? view.ranges.map(range => view.full.slice(range.start, range.end + 1)) : view.parsed.groups;
}
function coversSide(lines, side, total) {
  if (typeof total !== 'number') return false;
  let expected = 1;
  for (const line of lines) {
    if (line[side] === null) continue;
    if (line[side] !== expected++) return false;
  }
  return expected === total + 1;
}
function wholeFileVisible(file, lines) {
  return coversSide(lines, 'old', file.old_lines) && coversSide(lines, 'new', file.new_lines);
}
function hasHiddenBelow(file, lines) {
  const old = lines.findLast(line => line.old !== null)?.old ?? 0;
  const current = lines.findLast(line => line.new !== null)?.new ?? 0;
  return old < file.old_lines || current < file.new_lines;
}
function syntaxTokens(file, view, groups) {
  const key = view.full ? view.ranges.map(range => `${range.start}:${range.end}`).join(',') : 'patch';
  if (view.syntaxKey !== key) {
    view.syntax = ReviewHighlight.tokens(file.path, groups);
    view.syntaxKey = key;
  }
  return view.syntax;
}
function mergeRanges(ranges) {
  const result = [];
  for (const range of ranges.sort((a, b) => a.start - b.start)) {
    const previous = result.at(-1);
    if (previous && range.start <= previous.end + 1) previous.end = Math.max(previous.end, range.end);
    else result.push({ ...range });
  }
  return result;
}
async function loadFull(file, view) {
  if (view.full) return;
  if (!view.loading) view.loading = (async () => {
    const activePr = review.source === 'pr' ? review.pr_url.split('/').at(-1) : '0';
    const params = new URLSearchParams(selectionQuery(review.repo, loadedBase, activePr));
    params.set('path', file.path); params.set('context', '1000000');
    const result = await api(`api/diff?${params}`);
    const parsed = parsePatch(result.patch);
    const full = parsed.groups.flat();
    const keys = new Set(view.parsed.groups.flat().map(lineKey));
    const ranges = [];
    full.forEach((line, index) => { if (keys.has(lineKey(line))) ranges.push({ start: index, end: index }); });
    // Reject a moving file rather than silently presenting context from another version.
    for (const line of view.parsed.groups.flat()) {
      if (!full.some(candidate => lineKey(candidate) === lineKey(line) && candidate.text === line.text)) throw new Error('This file changed while expanding. Refresh all before expanding again.');
    }
    view.full = full; view.ranges = mergeRanges(ranges);
  })().finally(() => { view.loading = null; });
  await view.loading;
}
async function expand(file, boundary, direction, whole = false) {
  if (composer || dragSelection) { status('Finish selecting or editing your comment before expanding context.', true); return; }
  const view = views.get(file.path);
  try {
    status(`Loading context for ${file.path}…`);
    await loadFull(file, view);
    if (views.get(file.path) !== view || composer || dragSelection) return;
    if (whole) view.ranges = view.full.length ? [{ start: 0, end: view.full.length - 1 }] : [];
    else {
      const index = view.full.findIndex(line => lineKey(line) === boundary);
      const range = view.ranges.find(item => index >= item.start && index <= item.end);
      if (!range) throw new Error('Context moved; refresh the whole diff.');
      if (direction === 'above') range.start = Math.max(0, range.start - 20);
      else range.end = Math.min(view.full.length - 1, range.end + 20);
      view.ranges = mergeRanges(view.ranges);
    }
    const section = sections.get(file.path); const top = section.getBoundingClientRect().top;
    renderFile(file, section);
    $('#main').scrollTop += section.getBoundingClientRect().top - top;
    syncStatus();
  } catch (error) { status(error.message, true); }
}
function hunkLabel(group) {
  const old = group.filter(line => line.old !== null), current = group.filter(line => line.new !== null);
  return `@@ -${old[0]?.old ?? 0},${old.length} +${current[0]?.new ?? 0},${current.length} @@`;
}
function contextControl(direction, action) {
  const label = `Expand ${direction}`;
  const control = button(direction === 'above' ? '↑' : '↓', action);
  control.setAttribute('aria-label', label);
  control.title = `${label} · 20 lines`;
  return control;
}
function hunkRow(label, controls = []) {
  const row = el('div', `hunk${controls.length ? ' expand-gap' : ''}`);
  const gutter = el('span', 'hunk-controls');
  gutter.append(...controls);
  row.append(gutter, el('span', 'hunk-label', label));
  return row;
}
function rangeLines(comment, lines) {
  return lines.filter(line => line[comment.side] !== null && line[comment.side] >= comment.line && line[comment.side] <= endLine(comment));
}
function commentMatches(comment, lines) {
  if (comment.baseOid !== review.base_oid) return false;
  const range = rangeLines(comment, lines);
  return range.length === endLine(comment) - comment.line + 1 && range.map(line => line.text).join('\n') === comment.lineText;
}
function renderDiff() {
  const main = $('#main'); main.replaceChildren(); sections.clear();
  const files = visibleFiles();
  if (!files.length) main.append(el('div', 'empty', review.files.length ? 'No files match these filters. Hidden-file comments are still included in feedback.' : `No changes against ${review.base}.`));
  for (const file of files) {
    const section = el('section', 'review-file'); section.dataset.path = file.path;
    sections.set(file.path, section); main.append(section); renderFile(file, section);
  }
}
function renderFile(file, section) {
  section.replaceChildren();
  const view = views.get(file.path), groups = groupsFor(view), lines = groups.flat();
  const syntax = syntaxTokens(file, view, groups);
  const heading = el('div', 'file-heading'), title = el('h2');
  const pathButton = button(file.path, () => copyPath(file.path), 'file-path');
  pathButton.title = 'Copy file path'; pathButton.setAttribute('aria-label', `Copy path: ${file.path}`);
  title.append(pathButton); heading.append(title);
  const actions = el('div', 'actions');
  actions.append(countBadge(view, 'file-counts'));
  if (lines.length && !wholeFileVisible(file, lines)) actions.append(button('Whole file', () => expand(file, null, null, true)));
  heading.append(actions); section.append(heading);
  if (view.parsed.notes.length) section.append(el('div', 'metadata', view.parsed.notes.join('\n')));
  const diff = el('div', 'diff'); section.append(diff);
  const fileComments = comments.filter(comment => comment.path === file.path);
  groups.forEach((group, groupIndex) => {
    if (!group.length) return;
    const first = group[0];
    const hasAbove = groupIndex > 0 || (view.full ? view.ranges[0].start > 0 : (first.old ?? first.new) > 1);
    const controls = [];
    if (hasAbove) {
      if (groupIndex > 0) controls.push(contextControl('below', () => expand(file, lineKey(groups[groupIndex - 1].at(-1)), 'below')));
      controls.push(contextControl('above', () => expand(file, lineKey(first), 'above')));
    }
    diff.append(hunkRow(hunkLabel(group), controls));
    for (const line of group) {
      const row = el('div', `line ${line.kind}`);
      row.dataset.old = line.old ?? ''; row.dataset.new = line.new ?? ''; row.dataset.path = file.path;
      row.diffLine = line; row.diffFile = file;
      const code = el('span', 'code'), tokens = syntax.get(line);
      if (!tokens) code.textContent = line.text;
      else for (const token of tokens) code.append(token.className ? el('span', token.className, token.text) : document.createTextNode(token.text));
      const oldNumber = el('span', 'number'), newNumber = el('span', 'number'), sign = el('span', 'sign');
      oldNumber.dataset.label = line.old ?? ''; newNumber.dataset.label = line.new ?? '';
      sign.dataset.label = line.kind === 'add' ? '+' : line.kind === 'del' ? '−' : ' ';
      for (const decoration of [oldNumber, newNumber, sign]) decoration.setAttribute('aria-hidden', 'true');
      row.append(oldNumber, newNumber, sign, code);
      row.tabIndex = 0; row.role = 'button';
      row.setAttribute('aria-label', `${file.path}: Comment on ${line.kind === 'del' ? 'old' : 'new'} line ${line.kind === 'del' ? line.old : line.new}`);
      const open = event => selectLine(file, line, row, event.shiftKey, event.target === row.firstElementChild && line.old !== null ? 'old' : undefined);
      row.onclick = event => { if (!event.detail || event.shiftKey) open(event); };
      row.onmousedown = event => {
        if (event.shiftKey) { event.preventDefault(); window.getSelection()?.removeAllRanges(); }
      };
      row.onkeydown = event => { if (event.key === 'Enter' || event.key === ' ') { event.preventDefault(); open(event); } };
      diff.append(row);
      for (const comment of fileComments.filter(comment => line[comment.side] === endLine(comment) && commentMatches(comment, lines))) diff.append(commentCard(comment, false));
    }
  });
  if (groups.length && hasHiddenBelow(file, lines)) {
    diff.append(hunkRow('Expand unchanged lines', [
      contextControl('below', () => expand(file, lineKey(groups.at(-1).at(-1)), 'below')),
    ]));
  }
  if (!lines.length && !view.parsed.notes.length) diff.append(el('div', 'metadata', 'No textual diff. File metadata may have changed.'));
  const detached = fileComments.filter(comment => !commentMatches(comment, lines));
  if (detached.length) {
    section.append(el('div', 'metadata', 'Comments outside visible context or on an earlier version (original anchors preserved):'));
    for (const comment of detached) section.append(commentCard(comment, true));
  }
}
async function selectLine(file, line, row, extend, preferredSide) {
  if (extend && composer) {
    const anchor = composer.anchor;
    if (anchor.path !== file.path || line[anchor.side] === null) { status('Select a range in the same file and on the same side of the diff.', true); return; }
    const start = Math.min(composer.originLine, line[anchor.side]), end = Math.max(composer.originLine, line[anchor.side]);
    const view = views.get(file.path);
    try {
      let available = view.full || view.parsed.groups.flat();
      if (rangeLines({ ...anchor, line: start, endLine: end }, available).length !== end - start + 1) {
        await loadFull(file, view); available = view.full;
        if (!composer || composer.anchor !== anchor) return;
      }
      const selectedLines = rangeLines({ ...anchor, line: start, endLine: end }, available);
      if (selectedLines.length !== end - start + 1) throw new Error('Cannot select this range; refresh the diff first.');
      composer.anchor = { ...anchor, line: start, endLine: end, lineText: selectedLines.map(item => item.text).join('\n') };
      composer.querySelector('.comment-meta').textContent = anchorLabel(composer.anchor);
      highlightSelection();
    } catch (error) { status(error.message, true); }
    return;
  }
  if (composer) { composer.querySelector('textarea').focus(); status('Shift-click a line to extend this comment, or save/cancel it first.', true); return; }
  const side = preferredSide || (line.kind === 'del' ? 'old' : 'new');
  openComposer(row, { path: file.path, side, line: line[side], endLine: line[side], lineText: line.text, baseOid: review.base_oid });
}
function highlightSelection() {
  for (const row of document.querySelectorAll('.line')) {
    const anchor = dragSelection?.anchor || composer?.anchor, number = anchor ? Number(row.dataset[anchor.side]) : 0;
    row.classList.toggle('selected-line', !!anchor && row.dataset.path === anchor.path && number >= anchor.line && number <= endLine(anchor));
  }
}
function updateDragTarget() {
  const drag = dragSelection;
  if (!drag) return;
  const bounds = $('#main').getBoundingClientRect();
  const headerHeight = sections.get(drag.file.path).querySelector('.file-heading').offsetHeight;
  const y = Math.max(bounds.top + headerHeight + 1, Math.min(bounds.bottom - 2, drag.y));
  const row = document.elementFromPoint(drag.x, y)?.closest('.line');
  if (!row || row.diffFile.path !== drag.file.path || row.diffLine[drag.side] === null) return;
  const number = row.diffLine[drag.side];
  if (number === drag.endLine) return;
  drag.endLine = number; drag.endRow = row;
  drag.anchor.line = Math.min(drag.startLine, number);
  drag.anchor.endLine = Math.max(drag.startLine, number);
  highlightSelection();
}
function scrollDrag() {
  const drag = dragSelection;
  if (!drag) return;
  const main = $('#main'), bounds = main.getBoundingClientRect();
  if (drag.x >= bounds.left && drag.x <= bounds.right) {
    const speed = drag.y < bounds.top + 32 ? -12 : drag.y > bounds.bottom - 32 ? 12 : 0;
    if (speed) {
      main.scrollTop += speed;
      updateDragTarget();
    }
  }
  dragFrame = requestAnimationFrame(scrollDrag);
}
function clearDrag() {
  const drag = dragSelection;
  dragSelection = null; cancelAnimationFrame(dragFrame); dragFrame = null;
  document.body.classList.remove('selecting-lines');
  const main = $('#main');
  if (drag && main.hasPointerCapture(drag.pointerId)) main.releasePointerCapture(drag.pointerId);
  highlightSelection();
  return drag;
}
function startDrag(event) {
  if (!event.isPrimary || event.button !== 0 || event.shiftKey) return;
  const gutter = event.target.closest('.number, .sign'), row = gutter?.closest('.line');
  if (!row) return;
  const preferred = gutter === row.firstElementChild ? 'old' : 'new';
  const side = row.diffLine[preferred] === null ? (preferred === 'old' ? 'new' : 'old') : preferred;
  event.preventDefault();
  if (composer) {
    composer.querySelector('textarea').focus();
    status('Save or cancel this comment before dragging a new range, or Shift-click to extend it.', true);
    return;
  }
  const number = row.diffLine[side];
  dragSelection = {
    file: row.diffFile, side, startLine: number, endLine: number,
    startRow: row, endRow: row, pointerId: event.pointerId, x: event.clientX, y: event.clientY,
    anchor: { path: row.diffFile.path, side, line: number, endLine: number },
  };
  window.getSelection()?.removeAllRanges();
  $('#main').setPointerCapture(event.pointerId);
  document.body.classList.add('selecting-lines');
  highlightSelection(); dragFrame = requestAnimationFrame(scrollDrag);
}
async function finishDrag(event) {
  if (!dragSelection || event.pointerId !== dragSelection.pointerId) return;
  dragSelection.x = event.clientX; dragSelection.y = event.clientY; updateDragTarget();
  const drag = clearDrag();
  await selectLine(drag.file, drag.startRow.diffLine, drag.startRow, false, drag.side);
  if (drag.endLine !== drag.startLine) await selectLine(drag.file, drag.endRow.diffLine, drag.endRow, true, drag.side);
}
function selectedCodeRange() {
  const selection = window.getSelection();
  if (!selection?.rangeCount || selection.isCollapsed) return null;
  const range = selection.getRangeAt(0);
  const codeAt = node => (node.nodeType === Node.ELEMENT_NODE ? node : node.parentElement)?.closest('.code');
  return codeAt(range.startContainer) || codeAt(range.endContainer) ? range : null;
}
function copySelectedCode(event) {
  if ((event.target instanceof Element && event.target.closest('textarea, input')) || !event.clipboardData) return;
  const range = selectedCodeRange();
  if (!range) return;
  const parts = [];
  for (const code of $('#main').querySelectorAll('.code')) {
    if (!range.intersectsNode(code)) continue;
    const selected = document.createRange(); selected.selectNodeContents(code);
    if (code.contains(range.startContainer)) selected.setStart(range.startContainer, range.startOffset);
    if (code.contains(range.endContainer)) selected.setEnd(range.endContainer, range.endOffset);
    parts.push(selected.toString());
  }
  if (!parts.length) return;
  event.clipboardData.setData('text/plain', parts.join('\n'));
  event.preventDefault();
}
function openComposer(after, anchor, existing) {
  if (composer) { composer.querySelector('textarea').focus(); return; }
  const box = el('form', 'composer'); box.anchor = { ...anchor }; box.originLine = anchor.line;
  box.append(el('div', 'comment-meta', anchorLabel(anchor)), el('div', 'range-hint', 'Drag line numbers or Shift-click to select a comment range. Drag code text to copy it.'));
  const input = el('textarea'); input.placeholder = 'Leave feedback for the coding agent…'; input.setAttribute('aria-label', 'Comment'); input.value = existing?.body || '';
  const actions = el('div', 'actions'); actions.append(el('span', 'hint', '⌘/Ctrl + Enter to save'));
  const cancel = button('Cancel', () => { box.remove(); composer = null; highlightSelection(); updateCount(); syncStatus(); });
  const save = button('Save comment', () => {}, 'primary'); save.type = 'submit';
  box.onsubmit = event => {
    event.preventDefault(); if (!input.value.trim()) { input.focus(); return; }
    const entry = { ...box.anchor, id: existing?.id || Array.from(crypto.getRandomValues(new Uint32Array(4)), n => n.toString(16).padStart(8, '0')).join(''), body: input.value.trim() };
    if (existing) comments = comments.map(comment => comment.id === existing.id ? entry : comment); else comments.push(entry);
    composer = null; const saved = persist(); renderDiff(); if (saved) syncStatus();
  };
  input.onkeydown = event => { if ((event.metaKey || event.ctrlKey) && event.key === 'Enter') { event.preventDefault(); box.requestSubmit(); } if (event.key === 'Escape') cancel.click(); };
  actions.append(cancel, save); box.append(input, actions); after.after(box); composer = box; updateCount(); input.focus(); highlightSelection(); syncStatus();
}
function commentCard(comment, detached, inDialog = false) {
  const card = el('div', 'comment-card');
  card.append(el('div', `comment-meta${detached ? ' stale' : ''}`, `${anchorLabel(comment)}${detached ? ' · original anchor' : ''}`));
  card.append(el('pre', 'captured-content', `Content: ${comment.lineText}`), el('p', '', comment.body));
  const actions = el('div', 'actions');
  actions.append(button('Edit', () => {
    if (inDialog) $('#comments-dialog').close();
    if (composer) { composer.querySelector('textarea').focus(); return; }
    if (inDialog) {
      // Hidden/deleted-file comments remain editable without changing the current filters.
      const after = sections.get(comment.path)?.querySelector('.file-heading') || $('#main').firstChild;
      openComposer(after, comment, comment);
    } else openComposer(card, comment, comment);
  }), button('Delete', () => {
    comments = comments.filter(item => item.id !== comment.id); persist();
    if (!composer) renderDiff(); if (inDialog) showComments(false);
  }));
  card.append(actions); return card;
}
function showComments(open = true) {
  const list = $('#comments-list'); list.replaceChildren();
  if (!comments.length) list.append(el('p', '', 'No comments yet. Click a line in the diff; shift-click another to select a range.'));
  for (const comment of comments) list.append(commentCard(comment, true, true));
  if (open) $('#comments-dialog').showModal();
}
function feedback() {
  const entries = comments.map((comment, index) => `${index + 1}. ${anchorLabel(comment)}\nContent: ${comment.lineText}\nComment: ${comment.body.trim()}`).join('\n\n');
  return `${review.pr_url ? `Review: ${review.pr_url}\n\n` : ''}${entries}\n\nNote: comments may be on stale copies of the files. Line numbers and content refer to the version reviewed.`;
}
function applyFilters() {
  if (!review) return;
  if (composer || dragSelection) { status('Finish selecting or editing your comment before changing filters.', true); return; }
  renderFiles(); renderDiff();
}
async function switchSource(pr) {
  if (composer || dragSelection) { status('Finish selecting or editing your comment before switching review source.', true); return; }
  if (storageError && comments.length) { status('Copy your in-memory comments before switching sources; browser storage is unavailable.', true); return; }
  sourcePr = pr; requestVersion++;
  writeSelectionUrl();
  await refresh();
}
function updateRepoLabel() {
  const path = review?.repo || repoPath;
  $('#choose-repo').textContent = path ? `${path.split(/[\\/]/).filter(Boolean).at(-1) || path} ▾` : 'Choose repository…';
  $('#choose-repo').title = path ? `Choose repository · ${path}` : 'Choose a local repository';
}
function updateRepoControls() {
  const opening = !!pendingRepoSelection;
  $('#repo-use').disabled = !repoDirectory?.repo_root || busy || opening || repoBrowsing;
  $('#repo-parent').disabled = !repoDirectory?.parent || opening || repoBrowsing;
  $('#repo-base').disabled = opening;
  for (const control of $('#repo-browse-form').querySelectorAll('input, button')) {
    if (control.id !== 'repo-parent') control.disabled = opening || repoBrowsing;
  }
  for (const control of $('#repo-folders').querySelectorAll('button')) control.disabled = opening || repoBrowsing;
}
async function browseRepository(path) {
  const version = ++repoBrowseVersion;
  repoDirectory = null; repoBrowsing = true; updateRepoControls();
  $('#repo-error').textContent = ''; $('#repo-folders').replaceChildren();
  $('#repo-browser-status').textContent = 'Loading folders…';
  try {
    const folder = await api(`api/repos${path ? `?${new URLSearchParams({ path })}` : ''}`);
    if (version !== repoBrowseVersion) return;
    repoDirectory = folder;
    $('#repo-path').value = folder.path; $('#repo-location').textContent = folder.path;
    $('#repo-hint').textContent = folder.repo_root ? `Repository: ${folder.repo_root}` : 'This folder is not inside a Git checkout. Open a folder below or enter another path.';
    for (const directory of folder.directories) {
      const entry = button('', () => browseRepository(directory.path), 'repo-folder');
      entry.append(el('span', 'folder-chevron', '›'), el('span', 'folder-name', directory.name));
      if (directory.is_repo) entry.append(el('span', 'repo-marker', 'Git'));
      entry.title = directory.path; $('#repo-folders').append(entry);
    }
    $('#repo-browser-status').textContent = `${folder.directories.length} folder${folder.directories.length === 1 ? '' : 's'}`;
  } catch (error) {
    if (version !== repoBrowseVersion) return;
    $('#repo-browser-status').textContent = ''; $('#repo-error').textContent = error.message;
    $('#repo-hint').textContent = 'Enter a valid folder path, or use Home.';
  } finally {
    if (version === repoBrowseVersion) { repoBrowsing = false; updateRepoControls(); }
  }
}
async function openRepositoryPicker() {
  if (composer || dragSelection) { status('Save or cancel the open comment before switching repositories.', true); return; }
  if (storageError && comments.length) { status('Copy your in-memory comments before switching repositories; browser storage is unavailable.', true); return; }
  if (!serverConfig || $('#repo-dialog').open) return;
  if (selectedCodeRange()) window.getSelection().removeAllRanges();
  $('#repo-base').value = loadedBase || baseRef || serverConfig.base;
  $('#repo-dialog').showModal();
  await browseRepository(review?.repo || repoPath || serverConfig.browse_path);
}
function cancelRepositoryPicker() {
  repoBrowseVersion++; repoBrowsing = false;
  if (pendingRepoSelection) {
    const previous = pendingRepoSelection.previous;
    repoPath = previous.repo; baseRef = previous.base; sourcePr = previous.pr;
    pendingRepoSelection = null; requestVersion++;
    if (review) syncStatus(); else status('Choose a repository to begin.');
  }
  updateRepoControls();
}
async function useRepository(event) {
  event.preventDefault();
  if (!repoDirectory?.repo_root || busy || pendingRepoSelection) return;
  const base = $('#repo-base').value.trim();
  if (!base) { $('#repo-error').textContent = 'Enter a local base ref, such as main or master.'; return; }
  const selection = { previous: { repo: repoPath, base: baseRef, pr: sourcePr } };
  pendingRepoSelection = selection;
  repoPath = repoDirectory.repo_root; baseRef = base;
  if (review) sourcePr = '0';
  requestVersion++;
  $('#repo-error').textContent = ''; $('#repo-browser-status').textContent = 'Opening repository…';
  updateRepoControls();
  const opened = await refresh();
  if (pendingRepoSelection !== selection) return;
  pendingRepoSelection = null;
  if (opened) $('#repo-dialog').close();
  else {
    repoPath = selection.previous.repo; baseRef = selection.previous.base; sourcePr = selection.previous.pr;
    requestVersion++;
    $('#repo-error').textContent = lastRefreshError || 'Could not open this repository.';
    $('#repo-browser-status').textContent = '';
  }
  updateRepoControls();
}
async function initializeRepository() {
  try {
    serverConfig = await api('api/config');
    if (!baseRef) baseRef = serverConfig.base;
    if (sourcePr === null) sourcePr = String(serverConfig.pr ?? 0);
    if (!repoPath && serverConfig.repo) repoPath = serverConfig.repo;
    $('#choose-repo').disabled = false; updateRepoLabel();
    $('#pr-number').value = sourcePr === '0' ? '' : sourcePr;
    if (!repoPath && !serverConfig.repo) {
      $('#main').replaceChildren(el('div', 'empty', 'Choose a Git repository to begin reviewing.'));
      status('Choose a repository to begin.');
      await openRepositoryPicker();
    } else await refresh();
  } catch (error) { status(`Could not load repository settings: ${error.message}. Reload to retry.`, true); }
}
$('#choose-repo').onclick = openRepositoryPicker;
$('#repo-browse-form').onsubmit = event => { event.preventDefault(); browseRepository($('#repo-path').value.trim()); };
$('#repo-select-form').onsubmit = useRepository;
$('#repo-parent').onclick = () => { if (repoDirectory?.parent) browseRepository(repoDirectory.parent); };
$('#repo-home').onclick = () => browseRepository('~');
$('#close-repo').onclick = () => { cancelRepositoryPicker(); $('#repo-dialog').close(); };
$('#repo-dialog').addEventListener('cancel', cancelRepositoryPicker);
let preferredSidebarWidth = null, sidebarResize = null;
try {
  const saved = Number(localStorage.getItem('local-review:sidebar-width'));
  if (Number.isFinite(saved) && saved > 0) preferredSidebarWidth = saved;
} catch { /* Resizing does not depend on storage. */ }
function sidebarWidthLimits() {
  const max = Math.max(120, Math.min(600, $('.layout').clientWidth - 240));
  return { min: Math.min(180, max), max };
}
function applySidebarWidth(width = preferredSidebarWidth ?? (matchMedia('(max-width: 760px)').matches ? 180 : 240)) {
  const { min, max } = sidebarWidthLimits();
  const current = Math.round(Math.max(min, Math.min(max, width)));
  $('.layout').style.setProperty('--sidebar-width', `${current}px`);
  const handle = $('#sidebar-resizer');
  handle.setAttribute('aria-valuemin', String(min)); handle.setAttribute('aria-valuemax', String(max));
  handle.setAttribute('aria-valuenow', String(current)); handle.setAttribute('aria-valuetext', `${current} pixels`);
  return current;
}
function saveSidebarWidth(width) {
  preferredSidebarWidth = applySidebarWidth(width);
  try { localStorage.setItem('local-review:sidebar-width', String(preferredSidebarWidth)); } catch { /* Keep the current in-memory width. */ }
}
function finishSidebarResize(cancel = false) {
  if (!sidebarResize) return;
  const resize = sidebarResize; sidebarResize = null;
  if (cancel) applySidebarWidth();
  else saveSidebarWidth(resize.width);
  document.body.classList.remove('resizing-sidebar');
  const handle = $('#sidebar-resizer');
  if (handle.hasPointerCapture(resize.pointerId)) handle.releasePointerCapture(resize.pointerId);
}
$('#sidebar-resizer').addEventListener('pointerdown', event => {
  if (!event.isPrimary || event.button !== 0 || $('#file-sidebar').hidden) return;
  event.preventDefault();
  const width = $('#file-sidebar').getBoundingClientRect().width;
  sidebarResize = { pointerId: event.pointerId, x: event.clientX, startWidth: width, width };
  event.currentTarget.focus({ preventScroll: true }); event.currentTarget.setPointerCapture(event.pointerId);
  document.body.classList.add('resizing-sidebar');
});
$('#sidebar-resizer').addEventListener('pointermove', event => {
  if (!sidebarResize || event.pointerId !== sidebarResize.pointerId) return;
  sidebarResize.width = applySidebarWidth(sidebarResize.startWidth + event.clientX - sidebarResize.x);
});
$('#sidebar-resizer').addEventListener('pointerup', event => {
  if (sidebarResize && event.pointerId === sidebarResize.pointerId) finishSidebarResize();
});
$('#sidebar-resizer').addEventListener('pointercancel', () => finishSidebarResize(true));
$('#sidebar-resizer').addEventListener('lostpointercapture', () => finishSidebarResize(true));
$('#sidebar-resizer').addEventListener('keydown', event => {
  if (event.key === 'Escape' && sidebarResize) { event.preventDefault(); finishSidebarResize(true); return; }
  const { min, max } = sidebarWidthLimits(), current = Number(event.currentTarget.getAttribute('aria-valuenow'));
  const step = event.shiftKey ? 50 : 10;
  const width = { ArrowLeft: current - step, ArrowRight: current + step, Home: min, End: max }[event.key];
  if (width === undefined) return;
  event.preventDefault(); saveSidebarWidth(width);
});
window.addEventListener('resize', () => applySidebarWidth());
window.addEventListener('blur', () => finishSidebarResize(true));
applySidebarWidth();
function setSidebarCollapsed(collapsed) {
  finishSidebarResize(true);
  $('.layout').classList.toggle('sidebar-collapsed', collapsed);
  $('#file-sidebar').hidden = collapsed;
  $('#sidebar-resizer').hidden = collapsed;
  const toggle = $('#toggle-sidebar'), label = collapsed ? 'Show file sidebar' : 'Hide file sidebar';
  toggle.setAttribute('aria-expanded', String(!collapsed));
  toggle.setAttribute('aria-label', label); toggle.title = label;
}
$('#toggle-sidebar').onclick = () => {
  const collapsed = !$('#file-sidebar').hidden;
  setSidebarCollapsed(collapsed);
  try { localStorage.setItem('local-review:sidebar-collapsed', String(collapsed)); } catch { /* Layout does not depend on storage. */ }
};
try { setSidebarCollapsed(localStorage.getItem('local-review:sidebar-collapsed') === 'true'); } catch { setSidebarCollapsed(false); }
$('#refresh').onclick = () => { if (selectedCodeRange()) window.getSelection().removeAllRanges(); refresh(); };
$('#filter').oninput = applyFilters;
$('#hide-tests').onchange = () => {
  if (composer || dragSelection) { $('#hide-tests').checked = !$('#hide-tests').checked; status('Finish selecting or editing your comment before changing filters.', true); return; }
  try { localStorage.setItem('local-review:hide-tests', String($('#hide-tests').checked)); } catch { /* Filtering does not depend on storage. */ }
  applyFilters();
};
$('#pr-form').onsubmit = event => { event.preventDefault(); const value = $('#pr-number').value.trim(); if (/^[1-9]\d*$/.test(value)) switchSource(value); else status('Enter a positive GitHub PR number.', true); };
$('#local').onclick = () => switchSource('0');
$('#pr-number').value = sourcePr && sourcePr !== '0' ? sourcePr : '';
try { $('#hide-tests').checked = localStorage.getItem('local-review:hide-tests') === 'true'; } catch { /* Use the visible default. */ }
$('#comments-button').onclick = () => showComments();
$('#close-comments').onclick = () => $('#comments-dialog').close();
$('#clear-comments').onclick = () => {
  if (!review || (!comments.length && !composer)) return;
  const saved = `${comments.length} saved comment${comments.length === 1 ? '' : 's'}`;
  const what = composer ? `${saved} and the open draft` : saved;
  const dialog = $('#clear-comments-dialog');
  dialog.dataset.storageKey = storageKey;
  $('#clear-comments-description').textContent = `Remove ${what} for this review, including hidden files? This cannot be undone. Other reviews are not affected.`;
  $('#clear-comments-error').hidden = true;
  dialog.showModal();
};
$('#confirm-clear-comments').onclick = () => {
  const dialog = $('#clear-comments-dialog');
  if (dialog.dataset.storageKey !== storageKey) {
    dialog.close(); status('The review changed. Click Clear comments again for the current review.', true); return;
  }
  try { localStorage.removeItem(storageKey); }
  catch (error) {
    $('#clear-comments-error').textContent = `Could not clear saved comments: ${error.message}. Nothing was cleared.`;
    $('#clear-comments-error').hidden = false; return;
  }
  clearDrag();
  if (composer) { composer.remove(); composer = null; }
  comments = []; storageError = ''; storageLoadFailed = false;
  dialog.close();
  updateCount(); renderDiff();
  if ($('#comments-dialog').open) showComments(false);
  status('Cleared all comments and drafts for this review.');
};
$('#file-sidebar').addEventListener('pointerdown', event => {
  if (event.target.closest('input')) return;
  event.preventDefault();
  event.target.closest('button')?.focus({ preventScroll: true });
});
$('#close-export').onclick = () => $('#export-dialog').close();
$('#copy').onclick = async () => {
  if (composer) {
    if (!composer.querySelector('textarea').value.trim()) { status('Save or cancel the open comment before copying feedback.', true); return; }
    composer.requestSubmit();
  }
  if (await copyText(feedback(), 'Feedback')) status(`Copied ${comments.length} comment${comments.length === 1 ? '' : 's'} with filenames, line ranges, and original content.`);
};
window.addEventListener('beforeunload', event => { if (composer?.querySelector('textarea').value.trim()) { event.preventDefault(); event.returnValue = ''; } });
$('#main').addEventListener('pointerdown', startDrag);
document.addEventListener('copy', copySelectedCode);
$('#main').addEventListener('pointermove', event => {
  if (!dragSelection || event.pointerId !== dragSelection.pointerId) return;
  dragSelection.x = event.clientX; dragSelection.y = event.clientY; updateDragTarget();
});
$('#main').addEventListener('pointerup', finishDrag);
$('#main').addEventListener('pointercancel', () => clearDrag());
$('#main').addEventListener('lostpointercapture', () => { if (dragSelection) clearDrag(); });
window.addEventListener('blur', () => clearDrag());
document.addEventListener('keydown', event => {
  if (event.key === 'Escape' && dragSelection) { event.preventDefault(); clearDrag(); }
});
let lastPoll = 0;
setInterval(() => {
  const delay = review?.source === 'pr' ? 30000 : 2500;
  if (serverConfig && (repoPath || serverConfig.repo) && $('#live').checked && !document.hidden && !busy && !composer && !dragSelection && !selectedCodeRange() && Date.now() - lastPoll >= delay) { lastPoll = Date.now(); refresh(); }
}, 2500);
document.addEventListener('visibilitychange', () => {
  if (document.hidden) clearDrag();
  else if (serverConfig && (repoPath || serverConfig.repo) && $('#live').checked && !composer && !dragSelection && !selectedCodeRange()) refresh();
});
updateCount();
initializeRepository();
