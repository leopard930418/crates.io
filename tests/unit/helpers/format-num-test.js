import { module, test } from 'qunit';

import { formatNum } from '../../../helpers/format-num';

module('Unit | Helper | format-num', function () {
  test('it works', function (assert) {
    assert.equal(formatNum(42), '42');
    assert.equal(formatNum(0), '0');
    assert.equal(formatNum(1000), '1,000');
  });
});
