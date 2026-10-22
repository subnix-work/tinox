// Run with: node --test tests/ui/datagrid.test.cjs
// Exercise the shipped JS through WebSocket messages. This small DOM double
// checks tree placement and event wiring; it does not simulate CSS layout.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const test = require('node:test');
const vm = require('node:vm');

class Element {
  constructor(tag) {
    this.tagName = tag.toUpperCase();
    this.children = [];
    this.attributes = {};
    this.dataset = {};
    this.style = {};
    this.className = '';
    this.listeners = {};
    this.text = '';
  }
  setAttribute(key, value) { this.attributes[key] = value; }
  getAttribute(key) { return this.attributes[key] ?? null; }
  get rows() { return this.children.filter(el => el.tagName === 'TR'); }
  get cells() { return this.children.filter(el => ['TH', 'TD'].includes(el.tagName)); }
  get textContent() { return this.text + this.children.map(el => el.textContent).join(''); }
  set textContent(value) { this.replaceChildren(); this.text = value; }
  appendChild(el) { return this.insertBefore(el, null); }
  insertBefore(el, before) {
    assert.ok(before === null || this.children.includes(before));
    if (el.parentNode) el.parentNode.removeChild(el);
    this.children.splice(before === null ? this.children.length : this.children.indexOf(before), 0, el);
    el.parentNode = this;
    return el;
  }
  removeChild(el) {
    assert.ok(this.children.includes(el));
    this.children.splice(this.children.indexOf(el), 1);
    el.parentNode = null;
  }
  replaceChild(el, old) { this.insertBefore(el, old); this.removeChild(old); }
  replaceChildren(...children) {
    for (const child of [...this.children]) this.removeChild(child);
    for (const child of children) this.appendChild(child);
  }
  querySelector(selector) {
    const id = /^\[data-tinox-id="([^"]+)"\]$/.exec(selector)?.[1];
    assert.ok(id, `unsupported selector: ${selector}`);
    for (const child of this.children) {
      if (child.getAttribute('data-tinox-id') === id) return child;
      const found = child.querySelector(selector);
      if (found) return found;
    }
    return null;
  }
  addEventListener(type, handler) { this.listeners[type] = handler; }
}

function client() {
  const assets = fs.readFileSync(path.join(__dirname,
    '../../crates/tinox-core-ext/ui/tinox/core/ui/Assets.tnx'), 'utf8');
  const js = assets.match(/fnc clientJs\(\) -> String\s*\{\s*return r#"([\s\S]*?)"#;/)[1];
  const mount = new Element('div');
  let socket;
  const sent = [];
  vm.runInNewContext(js, {
    document: {
      getElementById: () => mount,
      createElement: tag => new Element(tag),
      createTextNode: text => Object.assign(new Element('#text'), { text }),
      addEventListener() {},
    },
    window: { location: { protocol: 'http:', hostname: 'localhost', pathname: '/' } },
    WebSocket: class {
      static OPEN = 1;
      constructor() { socket = this; this.readyState = 1; }
      send(message) { sent.push(message); }
    },
  });
  return {
    send: message => socket.onmessage({ data: JSON.stringify(message) }),
    find: id => mount.querySelector(`[data-tinox-id="${id}"]`),
    sent,
  };
}

const node = (type, id, children = [], text = '', props = {}) =>
  ({ type, id, children, text, props, visible: true });
function row(i, header = false) {
  return node('GridRow', `row-${i}`, [
    node('GridCell', `select-${i}`, header ? [] : [
      node('Checkbox', `checkbox-${i}`, [], '', { checked: 'false' }),
    ], header ? 'Select' : '', { header: String(header) }),
    ...['subnix-work/tinox', String(i), `Bug ${i} — long issue title`].map((text, col) =>
      node('GridCell', `cell-${i}-${col}`, [], text, { header: String(header) })),
  ]);
}
function init(count) {
  const app = client();
  app.send({ kind: 'init', root: node('DataGrid', 'grid', [
    row('header', true), ...Array.from({ length: count }, (_, i) => row(i)),
  ]) });
  return app;
}
const insert = (parentId, index, child) => ({ op: 'insert', parentId, index, node: child });

test('all 29 rows share the table on initial render and after incremental growth', () => {
  for (const count of [10, 29]) {
    const app = init(count);
    const firstRow = app.find('row-0');
    const checkbox = app.find('checkbox-0');
    app.send({ kind: 'patch', ops: Array.from({ length: 29 - count }, (_, i) =>
      insert('grid', count + i + 1, row(count + i))) });
    const table = firstRow.parentNode;
    assert.equal(table.tagName, 'TABLE');
    assert.equal(table.rows.length, 30);
    assert.deepEqual(table.rows.map(el => el.getAttribute('data-tinox-id')),
      ['row-header', ...Array.from({ length: 29 }, (_, i) => `row-${i}`)]);
    for (let i = 0; i < 29; i++) {
      assert.equal(app.find(`row-${i}`).parentNode, table);
      assert.equal(app.find(`row-${i}`).cells.length, 4);
      assert.equal(app.find(`checkbox-${i}`).parentNode.tagName, 'TD');
    }
    assert.equal(app.find('row-0'), firstRow);
    assert.equal(app.find('checkbox-0'), checkbox);
    checkbox.checked = true;
    checkbox.listeners.change();
    assert.deepEqual(JSON.parse(app.sent.at(-1)), { kind: 'event', id: 'checkbox-0', value: 'true' });
    const lastCheckbox = app.find('checkbox-28');
    lastCheckbox.checked = true;
    lastCheckbox.listeners.change();
    assert.deepEqual(JSON.parse(app.sent.at(-1)), { kind: 'event', id: 'checkbox-28', value: 'true' });
    // The toolbar stays outside the table's scroll container.
    const grid = app.find('grid');
    assert.equal(grid.children[0].className, 'tinox-datagrid-toolbar');
    assert.equal(table.parentNode.className, 'tinox-datagrid-scroll');
    assert.equal(table.parentNode.parentNode, grid);
    assert.equal(grid.children.length, 2);
  }
});

test('indexed insertion, replacement, update and removal remain inside the table', () => {
  const app = init(10);
  app.send({ kind: 'patch', ops: [insert('grid', 2, row('middle')),
    insert('grid', 0, row('first'))] });
  const table = app.find('row-0').parentNode;
  assert.equal(table.rows[0], app.find('row-first'));
  assert.equal(table.rows[3], app.find('row-middle'));
  app.send({ kind: 'patch', ops: [
    { op: 'replace', id: 'row-middle', node: row('replacement') },
    { op: 'update', id: 'cell-replacement-0', text: 'Updated', props: {} },
    { op: 'remove', id: 'row-first' },
    { op: 'remove', id: 'row-9' },
  ] });
  assert.equal(app.find('row-replacement').parentNode, table);
  assert.equal(app.find('cell-replacement-0').textContent, 'Updated');
  assert.equal(table.rows.length, 11);
  assert.equal(app.find('row-first'), null);
  assert.equal(app.find('row-9'), null);
});

test('inserts also respect other inner containers and ordinary component parents', () => {
  for (const type of ['VBox', 'Dialog', 'AccordionSection']) {
    const app = client();
    app.send({ kind: 'init', root: node(type, 'parent', [node('Label', 'initial')]) });
    const target = app.find('initial').parentNode;
    app.send({ kind: 'patch', ops: [insert('parent', 99, node('Label', 'new'))] });
    assert.equal(app.find('new').parentNode, target);
    assert.equal(target.children.at(-1), app.find('new'));
  }
});
