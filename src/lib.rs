mod db;
mod errors;
mod github;
mod handlers;
mod session;
mod views;

use worker::*;

#[event(fetch)]
pub async fn main(req: Request, env: Env, _ctx: Context) -> Result<Response> {
    console_error_panic_hook::set_once();
    Router::new()
        .get_async("/",                  handlers::index)
        .get_async("/healthz",           |_, _| async { Response::ok("ok") })

        // Owner: no login. They just ask to create a poll.
        .get_async ("/app/new",          handlers::new_poll_form)
        .post_async("/app/new",          handlers::create_poll)
        .get_async ("/app/installed",    handlers::install_callback)

        // Voter
        .get_async ("/p/:id",            handlers::poll_page)
        .get_async ("/p/:id/login",      handlers::voter_login)
        .post_async("/p/:id/vote",       handlers::submit_vote)
        .get_async ("/p/callback",       handlers::voter_callback)   // new, stable

        .post_async("/webhooks/github",  handlers::github_webhook)

        .run(req, env).await
}