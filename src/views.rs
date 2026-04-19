pub fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
     .replace('"', "&quot;").replace('\'', "&#39;")
}

pub fn page(title: &str, body: &str) -> String {
    format!(r#"<!doctype html><html><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>{}</title>
<style>
body{{font-family:system-ui,sans-serif;max-width:640px;margin:3rem auto;padding:0 1rem;color:#222}}
input,textarea,button{{font:inherit;padding:.5rem;width:100%;box-sizing:border-box;margin:.25rem 0}}
button{{background:#1f6feb;color:#fff;border:0;border-radius:6px;cursor:pointer;padding:.6rem}}
a.btn{{display:inline-block;background:#1f6feb;color:#fff;padding:.6rem 1rem;border-radius:6px;text-decoration:none}}
code{{background:#f3f3f3;padding:.1rem .3rem;border-radius:3px}}
</style></head><body>{}</body></html>"#, html_escape(title), body)
}

pub fn index_html() -> String {
    page("GitHub Polls", r#"
        <h1>GitHub Polls</h1>
        <p>Polls stored as CSV in your repo. GitHub users vote, GitHub stores results.</p>
        <p><a class="btn" href="/app/new">Create a poll</a></p>
    "#)
}

pub fn new_poll_form_html(err: Option<&str>) -> String {
    let err_html = err.map(|e| format!(r#"<p style="color:#b00">{}</p>"#, html_escape(e)))
                      .unwrap_or_default();
    page("New poll", &format!(r#"
        <h1>Create a poll</h1>
        {err}
        <form method="post" action="/app/new" autocomplete="off">
          <label>Question<br>
            <textarea name="question" required maxlength="500"
                placeholder="What is your favourite color?"></textarea>
          </label>
          <label>Repository (<code>owner/repo</code>)<br>
            <input name="repo" required pattern="[A-Za-z0-9._-]+/[A-Za-z0-9._-]+"
                placeholder="yourname/my-polls">
          </label>
          <p>If our GitHub App isn't installed on that repo yet,
             you'll be redirected to GitHub to install it.</p>
          <button type="submit">Create poll</button>
        </form>
    "#, err = err_html))
}

pub fn poll_created_html(poll_url: &str, commit_url: &str) -> String {
    page("Poll created", &format!(r#"
        <h1>✅ Poll created</h1>
        <p>Share this link:</p>
        <p><input readonly value="{url}"></p>
        <p><a href="{commit}">See the file on GitHub →</a></p>
    "#, url = html_escape(poll_url), commit = html_escape(commit_url)))
}

pub fn install_ok_html(poll_url: &str) -> String {
    page("Installed", &format!(r#"
        <h1>✅ App installed &amp; poll created</h1>
        <p>Share this link:</p>
        <p><input readonly value="{url}"></p>
    "#, url = html_escape(poll_url)))
}

pub fn poll_vote_html(q: &str, action: &str, login: &str, already: bool) -> String {
    if already {
        return page("Already voted", &format!(r#"
            <h1>Thanks, {login}</h1>
            <p>You already voted on: <b>{q}</b></p>"#,
            login = html_escape(login), q = html_escape(q)));
    }
    page("Vote", &format!(r#"
        <h1>{q}</h1>
        <p>Signed in as <b>{login}</b></p>
        <form method="post" action="{action}">
          <label>Your answer<br>
            <input name="response" required maxlength="200">
          </label>
          <button type="submit">Submit vote</button>
        </form>"#,
        q = html_escape(q), login = html_escape(login), action = html_escape(action)))
}

pub fn login_required_html(login_url: &str, q: &str) -> String {
    page("Sign in", &format!(r#"
        <h1>{q}</h1>
        <p>Please sign in with GitHub to vote.</p>
        <p><a class="btn" href="{u}">Sign in with GitHub</a></p>"#,
        q = html_escape(q), u = html_escape(login_url)))
}

pub fn error_html(msg: &str) -> String {
    page("Error", &format!(r#"<h1>Something went wrong</h1><p>{}</p>"#, html_escape(msg)))
}