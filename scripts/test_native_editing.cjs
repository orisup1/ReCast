const assert = require('node:assert/strict');
const {scenarios, matches} = require('../tests/native-editing.js');
for (const scenario of scenarios) {
    const actual = Object.fromEntries(['plain', 'rich', 'other'].map(key => [key, scenario.expected[key][0]]));
    actual.sent = scenario.expected.sent.map(messages => messages[0]);
    assert(matches(scenario, actual), scenario.name);
    assert(!matches(scenario, {...actual, other: actual.other + 'lost'}), scenario.name);
    assert(!matches(scenario, {...actual, sent: [...actual.sent, 'duplicate send']}), scenario.name);
}
console.log(`${scenarios.length} native fixture outcome checks passed`);
