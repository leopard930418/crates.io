use super::frontend_prelude::*;

use crate::models::{CrateOwner, CrateOwnerInvitation, OwnerKind};
use crate::schema::{crate_owner_invitations, crate_owners};
use crate::views::{EncodableCrateOwnerInvitation, InvitationResponse};

/// Handles the `GET /me/crate_owner_invitations` route.
pub fn list(req: &mut dyn RequestExt) -> EndpointResult {
    let user_id = req.authenticate()?.user_id();
    let conn = &*req.db_read_only()?;

    let crate_owner_invitations: Vec<CrateOwnerInvitation> = crate_owner_invitations::table
        .filter(crate_owner_invitations::invited_user_id.eq(user_id))
        .load(&*conn)?;
    let crate_owner_invitations = crate_owner_invitations
        .into_iter()
        .map(|i| EncodableCrateOwnerInvitation::from(i, conn))
        .collect();

    #[derive(Serialize)]
    struct R {
        crate_owner_invitations: Vec<EncodableCrateOwnerInvitation>,
    }
    Ok(req.json(&R {
        crate_owner_invitations,
    }))
}

#[derive(Deserialize)]
struct OwnerInvitation {
    crate_owner_invite: InvitationResponse,
}

/// Handles the `PUT /me/crate_owner_invitations/:crate_id` route.
pub fn handle_invite(req: &mut dyn RequestExt) -> EndpointResult {
    let mut body = String::new();
    req.body().read_to_string(&mut body)?;

    let crate_invite: OwnerInvitation =
        serde_json::from_str(&body).map_err(|_| bad_request("invalid json request"))?;

    let crate_invite = crate_invite.crate_owner_invite;
    let user_id = req.authenticate()?.user_id();
    let conn = &*req.db_conn()?;

    if crate_invite.accepted {
        accept_invite(req, conn, crate_invite, user_id)
    } else {
        decline_invite(req, conn, crate_invite, user_id)
    }
}

/// Handles the `PUT /me/crate_owner_invitations/accept/:token` route.
pub fn handle_invite_with_token(req: &mut dyn RequestExt) -> EndpointResult {
    let conn = req.db_conn()?;
    let req_token = &req.params()["token"];

    let crate_owner_invite: CrateOwnerInvitation = crate_owner_invitations::table
        .filter(crate_owner_invitations::token.eq(req_token))
        .first(&*conn)?;

    let invite_reponse = InvitationResponse {
        crate_id: crate_owner_invite.crate_id,
        accepted: true,
    };
    accept_invite(
        req,
        &conn,
        invite_reponse,
        crate_owner_invite.invited_user_id,
    )
}

fn accept_invite(
    req: &dyn RequestExt,
    conn: &PgConnection,
    crate_invite: InvitationResponse,
    user_id: i32,
) -> EndpointResult {
    use diesel::{delete, insert_into};

    conn.transaction(|| {
        let pending_crate_owner: CrateOwnerInvitation = crate_owner_invitations::table
            .find((user_id, crate_invite.crate_id))
            .first(&*conn)?;

        insert_into(crate_owners::table)
            .values(&CrateOwner {
                crate_id: crate_invite.crate_id,
                owner_id: user_id,
                created_by: pending_crate_owner.invited_by_user_id,
                owner_kind: OwnerKind::User as i32,
                email_notifications: true,
            })
            .on_conflict(crate_owners::table.primary_key())
            .do_update()
            .set(crate_owners::deleted.eq(false))
            .execute(conn)?;
        delete(crate_owner_invitations::table.find((user_id, crate_invite.crate_id)))
            .execute(conn)?;

        #[derive(Serialize)]
        struct R {
            crate_owner_invitation: InvitationResponse,
        }
        Ok(req.json(&R {
            crate_owner_invitation: crate_invite,
        }))
    })
}

fn decline_invite(
    req: &dyn RequestExt,
    conn: &PgConnection,
    crate_invite: InvitationResponse,
    user_id: i32,
) -> EndpointResult {
    use diesel::delete;

    delete(crate_owner_invitations::table.find((user_id, crate_invite.crate_id))).execute(conn)?;

    #[derive(Serialize)]
    struct R {
        crate_owner_invitation: InvitationResponse,
    }

    Ok(req.json(&R {
        crate_owner_invitation: crate_invite,
    }))
}
