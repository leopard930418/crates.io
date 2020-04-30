import { currentURL } from '@ember/test-helpers';
import { setupApplicationTest } from 'ember-qunit';
import { module, test } from 'qunit';

import setupMirage from '../helpers/setup-mirage';
import { visit } from '../helpers/visit-ignoring-abort';

module('Route | keyword', function (hooks) {
  setupApplicationTest(hooks);
  setupMirage(hooks);

  test("shows an error message if the keyword can't be found", async function (assert) {
    await visit('/keywords/unknown');
    assert.equal(currentURL(), '/');
    assert.dom('[data-test-flash-message]').hasText("Keyword 'unknown' does not exist");
  });

  test('server error causes the error page to be shown', async function (assert) {
    this.server.get('/api/v1/keywords/:keywordId', {}, 500);

    await visit('/keywords/error');
    assert.equal(currentURL(), '/keywords/error');
    assert.dom('[data-test-error-message]').includesText('GET /api/v1/keywords/error returned a 500');
  });
});
