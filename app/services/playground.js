import Service from '@ember/service';

import { dropTask } from 'ember-concurrency';
import { alias } from 'macro-decorators';

import ajax from '../utils/ajax';

export default class PlaygroundService extends Service {
  @alias('loadCratesTask.lastSuccessful.value') crates;

  @dropTask *loadCratesTask() {
    let response = yield ajax('https://play.rust-lang.org/meta/crates');
    return response.crates;
  }
}
