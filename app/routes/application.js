import Route from '@ember/routing/route';
import { inject as service } from '@ember/service';

export default Route.extend({
  flashMessages: service(),
  googleCharts: service(),
  session: service(),

  beforeModel() {
    // trigger the task, but don't wait for the result here
    this.session.loadUserTask.perform();

    // start loading the Google Charts JS library already
    this.googleCharts.load();
  },

  actions: {
    didTransition() {
      this.flashMessages.step();
    },
  },
});
