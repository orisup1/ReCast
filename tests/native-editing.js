'use strict';
const scenarios = [
    {name: 'Rapid Space then Enter', plain: '', rich: '', other: '',
     steps: 'In everyday typing, type recieve, Space, then Enter rapidly. Both intact receive and untouched recieve are safe outcomes; partial words are failures.',
     expected: {plain: ['receive \n', 'recieve \n'], rich: [''], other: [''], sent: []}},
    {name: 'Chat sends once', plain: '', rich: '', other: '',
     steps: 'In chat, type recieve then press Enter once. Exactly one intact message must appear. ReCast must not replay Enter.',
     expected: {plain: [''], rich: [''], other: [''], sent: [['receive', 'recieve']]}},
    {name: 'Select all, delete, restart', plain: 'old text שלום', rich: '', other: '',
     steps: 'In everyday typing, select all, Backspace, then type fresh start. No old text may reappear.',
     expected: {plain: ['fresh start'], rich: [''], other: [''], sent: []}},
    {name: 'Selection rescue and exact undo', plain: 'left akuo שלום🙂 right', rich: '', other: '',
     steps: 'Select only akuo. Double-tap Ctrl: expect שלום. Double-tap Ctrl again: expect akuo. Check text after undo. If browser accessibility cannot replace selections, record unsupported; unchanged text alone does not prove success.',
     expected: {plain: ['left akuo שלום🙂 right'], rich: [''], other: [''], sent: []}},
    {name: 'Focus interruption', plain: '', rich: '', other: 'untouched',
     steps: 'Type recieve and Space in everyday typing; immediately click the second field. Do not type there. Correction must never erase or alter untouched.',
     expected: {plain: ['receive ', 'recieve '], rich: [''], other: ['untouched'], sent: []}},
    {name: 'Manual-only coding', plain: '', rich: '', other: '',
     steps: 'Set this browser to Manual only. Type exactly: const recieve = "שלום"; Then press Space. No automatic rewrite. Restore app mode afterward.',
     expected: {plain: ['const recieve = "שלום"; '], rich: [''], other: [''], sent: []}},
    {name: 'Mixed-language preservation', plain: 'שלום API_URL https://example.test 🙂', rich: '', other: '',
     steps: 'Focus everyday typing at the end and press Space. Existing mixed-language text must remain intact.',
     expected: {plain: ['שלום API_URL https://example.test 🙂 '], rich: [''], other: [''], sent: []}},
];
function matches(scenario, actual) {
    return ['plain', 'rich', 'other'].every(key => scenario.expected[key].includes(actual[key]))
        && actual.sent.length === scenario.expected.sent.length
        && actual.sent.every((message, index) => scenario.expected.sent[index].includes(message));
}
if (typeof module !== 'undefined') module.exports = {scenarios, matches};
if (typeof document !== 'undefined') {
    const get = id => document.getElementById(id);
    const trace = [];
    const sent = [];
    let started;
    scenarios.forEach((scenario, index) => get('scenario').add(new Option(scenario.name, index)));
    const current = () => scenarios[Number(get('scenario').value)];
    const snapshot = () => ({plain: get('plain').value, rich: get('rich').innerText, other: get('other').value, sent: [...sent]});
    function record(event) {
        if (trace.length >= 2000) trace.shift();
        trace.push({ms: Math.round((performance.now() - started) * 100) / 100,
            target: event.target.id, type: event.type, key: event.key, inputType: event.inputType,
            text: event.target.isContentEditable ? event.target.innerText : event.target.value});
    }
    for (const id of ['plain', 'rich', 'other']) {
        for (const type of ['keydown', 'beforeinput', 'input', 'focus', 'blur']) get(id).addEventListener(type, record);
    }
    get('rich').addEventListener('keydown', event => {
        if (event.key === 'Enter' && !event.shiftKey && !event.isComposing) {
            event.preventDefault();
            sent.push(get('rich').innerText);
            get('rich').textContent = '';
            get('sent').textContent = JSON.stringify(sent);
        }
    });
    function reset() {
        const scenario = current();
        started = performance.now(); trace.length = 0; sent.length = 0;
        for (const id of ['plain', 'other']) get(id).value = scenario[id];
        get('rich').textContent = scenario.rich;
        get('sent').textContent = '';
        get('steps').textContent = scenario.steps;
        get('result').textContent = 'Not checked.';
    }
    get('reset').onclick = reset;
    get('scenario').onchange = reset;
    get('check').onclick = () => {
        get('result').textContent = matches(current(), snapshot()) ? 'Text matches an allowed outcome. Confirm the instructed actions occurred.' : 'FAIL: text differs from allowed outcomes.';
    };
    get('export').onclick = () => {
        const report = {scenario: current().name, notes: get('notes').value,
            date: new Date().toISOString(), browser: navigator.userAgent,
            actual: snapshot(), textMatches: matches(current(), snapshot()), trace};
        const url = URL.createObjectURL(new Blob([JSON.stringify(report, null, 2)], {type:'application/json'}));
        const link = document.createElement('a'); link.href = url; link.download = 'recast-native-check.json'; link.click();
        setTimeout(() => URL.revokeObjectURL(url), 1000);
    };
    reset();
}
