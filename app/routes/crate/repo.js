import Route from '@ember/routing/route';
import { inject as service } from '@ember/service';

export default Route.extend({
  flashMessages: service(),
  redirector: service(),

  redirect() {
    let crate = this.modelFor('crate');

    let repository = crate.get('repository');
    if (repository) {
      this.redirector.redirectTo(repository);
    } else {
      // Redirect to the crate's main page and show a flash error if
      // no repository is found
      let message = 'Crate does not supply a repository URL';
      this.flashMessages.queue(message);
      this.replaceWith('crate', crate);
    }
  },
});
