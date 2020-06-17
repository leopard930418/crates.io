import Route from '@ember/routing/route';
import { inject as service } from '@ember/service';

export default Route.extend({
  notifications: service(),

  queryParams: {
    page: { refreshModel: true },
    sort: { refreshModel: true },
  },

  async model(params) {
    const { team_id } = params;

    try {
      let team = await this.store.queryRecord('team', { team_id });

      params.team_id = team.get('id');
      params.include_yanked = 'n';
      let crates = await this.store.query('crate', params);

      return { crates, team };
    } catch (e) {
      if (e.errors.some(e => e.detail === 'Not Found')) {
        this.notifications.error(`Team '${params.team_id}' does not exist`);
        return this.replaceWith('index');
      }
    }
  },
});
