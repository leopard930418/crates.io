use chrono::NaiveDateTime;
use diesel::prelude::*;

use crate::schema::{crate_owner_invitations, crates, users};

/// The model representing a row in the `crate_owner_invitations` database table.
#[derive(Clone, Debug, PartialEq, Eq, Identifiable, Queryable)]
#[primary_key(invited_user_id, crate_id)]
pub struct CrateOwnerInvitation {
    pub invited_user_id: i32,
    pub invited_by_user_id: i32,
    pub crate_id: i32,
    pub created_at: NaiveDateTime,
    pub token: String,
    pub token_created_at: Option<NaiveDateTime>,
}

#[derive(Insertable, Clone, Copy, Debug)]
#[table_name = "crate_owner_invitations"]
pub struct NewCrateOwnerInvitation {
    pub invited_user_id: i32,
    pub invited_by_user_id: i32,
    pub crate_id: i32,
}

impl CrateOwnerInvitation {
    pub fn invited_by_username(&self, conn: &PgConnection) -> String {
        users::table
            .find(self.invited_by_user_id)
            .select(users::gh_login)
            .first(&*conn)
            .unwrap_or_else(|_| String::from("(unknown username)"))
    }

    pub fn crate_name(&self, conn: &PgConnection) -> String {
        crates::table
            .find(self.crate_id)
            .select(crates::name)
            .first(&*conn)
            .unwrap_or_else(|_| String::from("(unknown crate name)"))
    }
}
