use crate::controllers::prelude::*;

use crate::controllers::helpers::Paginate;
use crate::email;
use crate::util::bad_request;

use crate::models::{Email, Follow, NewEmail, User, Version};
use crate::schema::{crates, emails, follows, users, versions};
use crate::views::{EncodableMe, EncodableVersion};

/// Handles the `GET /me` route.
pub fn me(req: &mut dyn Request) -> CargoResult<Response> {
    // Changed to getting User information from database because in
    // src/tests/user.rs, when testing put and get on updating email,
    // request seems to be somehow 'cached'. When we try to get a
    // request from the /me route with the just updated user (call
    // this function) the user is the same as the initial GET request
    // and does not seem to get the updated user information from the
    // database
    // This change is not preferable, we'd rather fix the request,
    // perhaps adding `req.mut_extensions().insert(user)` to the
    // update_user route, however this somehow does not seem to work

    let id = req.user()?.id;
    let conn = req.db_conn()?;

    let (user, verified, email, verification_sent) = users::table
        .find(id)
        .left_join(emails::table)
        .select((
            users::all_columns,
            emails::verified.nullable(),
            emails::email.nullable(),
            emails::token_generated_at.nullable().is_not_null(),
        ))
        .first::<(User, Option<bool>, Option<String>, bool)>(&*conn)?;

    let verified = verified.unwrap_or(false);
    let verification_sent = verified || verification_sent;
    let user = User { email, ..user };

    Ok(req.json(&EncodableMe {
        user: user.encodable_private(verified, verification_sent),
    }))
}

/// Handles the `GET /me/updates` route.
pub fn updates(req: &mut dyn Request) -> CargoResult<Response> {
    use diesel::dsl::any;

    let user = req.user()?;
    let (offset, limit) = req.pagination(10, 100)?;
    let conn = req.db_conn()?;

    let followed_crates = Follow::belonging_to(user).select(follows::crate_id);
    let data = versions::table
        .inner_join(crates::table)
        .filter(crates::id.eq(any(followed_crates)))
        .order(versions::created_at.desc())
        .select((versions::all_columns, crates::name))
        .paginate(limit, offset)
        .load::<((Version, String), i64)>(&*conn)?;

    let more = data
        .get(0)
        .map(|&(_, count)| count > offset + limit)
        .unwrap_or(false);

    let versions = data
        .into_iter()
        .map(|((version, crate_name), _)| version.encodable(&crate_name))
        .collect();

    #[derive(Serialize)]
    struct R {
        versions: Vec<EncodableVersion>,
        meta: Meta,
    }
    #[derive(Serialize)]
    struct Meta {
        more: bool,
    }
    Ok(req.json(&R {
        versions,
        meta: Meta { more },
    }))
}

/// Handles the `PUT /user/:user_id` route.
pub fn update_user(req: &mut dyn Request) -> CargoResult<Response> {
    use self::emails::user_id;
    use self::users::dsl::{email, gh_login, users};
    use diesel::{insert_into, update};

    let mut body = String::new();
    req.body().read_to_string(&mut body)?;
    let user = req.user()?;
    let name = &req.params()["user_id"];
    let conn = req.db_conn()?;

    // need to check if current user matches user to be updated
    if &user.id.to_string() != name {
        return Err(human("current user does not match requested user"));
    }

    #[derive(Deserialize)]
    struct UserUpdate {
        user: User,
    }

    #[derive(Deserialize)]
    struct User {
        email: Option<String>,
    }

    let user_update: UserUpdate =
        serde_json::from_str(&body).map_err(|_| human("invalid json request"))?;

    if user_update.user.email.is_none() {
        return Err(human("empty email rejected"));
    }

    let user_email = user_update.user.email.unwrap();
    let user_email = user_email.trim();

    if user_email == "" {
        return Err(human("empty email rejected"));
    }

    conn.transaction(|| {
        update(users.filter(gh_login.eq(&user.gh_login)))
            .set(email.eq(user_email))
            .execute(&*conn)?;

        let new_email = NewEmail {
            user_id: user.id,
            email: user_email,
        };

        let token = insert_into(emails::table)
            .values(&new_email)
            .on_conflict(user_id)
            .do_update()
            .set(&new_email)
            .returning(emails::token)
            .get_result::<String>(&*conn)
            .map_err(|_| human("Error in creating token"))?;

        crate::email::send_user_confirm_email(user_email, &user.gh_login, &token)
            .map_err(|_| bad_request("Email could not be sent"))
    })?;

    #[derive(Serialize)]
    struct R {
        ok: bool,
    }
    Ok(req.json(&R { ok: true }))
}

/// Handles the `PUT /confirm/:email_token` route
pub fn confirm_user_email(req: &mut dyn Request) -> CargoResult<Response> {
    use diesel::update;

    let conn = req.db_conn()?;
    let req_token = &req.params()["email_token"];

    let updated_rows = update(emails::table.filter(emails::token.eq(req_token)))
        .set(emails::verified.eq(true))
        .execute(&*conn)?;

    if updated_rows == 0 {
        return Err(bad_request("Email belonging to token not found."));
    }

    #[derive(Serialize)]
    struct R {
        ok: bool,
    }
    Ok(req.json(&R { ok: true }))
}

/// Handles `PUT /user/:user_id/resend` route
pub fn regenerate_token_and_send(req: &mut dyn Request) -> CargoResult<Response> {
    use diesel::dsl::sql;
    use diesel::update;

    let user = req.user()?;
    let name = &req.params()["user_id"].parse::<i32>().ok().unwrap();
    let conn = req.db_conn()?;

    // need to check if current user matches user to be updated
    if &user.id != name {
        return Err(human("current user does not match requested user"));
    }

    conn.transaction(|| {
        let email = update(Email::belonging_to(user))
            .set(emails::token.eq(sql("DEFAULT")))
            .get_result::<Email>(&*conn)
            .map_err(|_| bad_request("Email could not be found"))?;

        email::send_user_confirm_email(&email.email, &user.gh_login, &email.token)
            .map_err(|_| bad_request("Error in sending email"))
    })?;

    #[derive(Serialize)]
    struct R {
        ok: bool,
    }
    Ok(req.json(&R { ok: true }))
}
