import { module, test } from 'qunit';

import fetch from 'fetch';

import { setupTest } from '../../helpers';
import setupMirage from '../../helpers/setup-mirage';

module('Mirage | Crates', function (hooks) {
  setupTest(hooks);
  setupMirage(hooks);

  module('GET /api/v1/crates/:id/owner_user', function () {
    test('returns 404 for unknown crates', async function (assert) {
      let response = await fetch('/api/v1/crates/foo/owner_user');
      assert.equal(response.status, 404);

      let responsePayload = await response.json();
      assert.deepEqual(responsePayload, { errors: [{ detail: 'Not Found' }] });
    });

    test('empty case', async function (assert) {
      this.server.create('crate', { name: 'rand' });

      let response = await fetch('/api/v1/crates/rand/owner_user');
      assert.equal(response.status, 200);

      let responsePayload = await response.json();
      assert.deepEqual(responsePayload, {
        users: [],
      });
    });

    test('returns the list of users that own the specified crate', async function (assert) {
      let user = this.server.create('user', { name: 'John Doe' });
      let crate = this.server.create('crate', { name: 'rand' });
      this.server.create('crate-ownership', { crate, user });

      let response = await fetch('/api/v1/crates/rand/owner_user');
      assert.equal(response.status, 200);

      let responsePayload = await response.json();
      assert.deepEqual(responsePayload, {
        users: [
          {
            id: 1,
            avatar: 'https://avatars1.githubusercontent.com/u/14631425?v=4',
            kind: 'user',
            login: 'john-doe',
            name: 'John Doe',
            url: 'https://github.com/john-doe',
          },
        ],
      });
    });
  });
});
