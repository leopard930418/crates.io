import Component from '@ember/component';

export default Component.extend({
  tagName: '',
  isAccepted: false,
  isDeclined: false,
  isError: false,
  inviteError: 'default error message',

  actions: {
    async acceptInvitation(invite) {
      invite.set('accepted', true);

      try {
        await invite.save();
        this.set('isAccepted', true);
      } catch (error) {
        this.set('isError', true);
        if (error.errors) {
          this.set('inviteError', `Error in accepting invite: ${error.errors[0].detail}`);
        } else {
          this.set('inviteError', 'Error in accepting invite');
        }
      }
    },

    async declineInvitation(invite) {
      invite.set('accepted', false);

      try {
        await invite.save();
        this.set('isDeclined', true);
      } catch (error) {
        this.set('isError', true);
        if (error.errors) {
          this.set('inviteError', `Error in declining invite: ${error.errors[0].detail}`);
        } else {
          this.set('inviteError', 'Error in declining invite');
        }
      }
    },
  },
});
