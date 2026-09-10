use serenity::{model::id::UserId, prelude::Context};

/// The user's name if it is already cached, without touching the network.
///
/// Exists so the voice tick path can take the cheap path when it is available
/// and defer only when it is not. Constitution Principle I forbids a network
/// call on that path, and a cache hit is not one.
pub fn resolve_user_name_cached(ctx: &Context, user_id: UserId) -> Option<String> {
    ctx.cache.user(user_id).map(|user| user.name.clone())
}

pub async fn resolve_user_name(ctx: &Context, user_id: UserId) -> String {
    if let Some(name) = resolve_user_name_cached(ctx, user_id) {
        return name;
    }

    match ctx.http.get_user(user_id).await {
        Ok(user) => user.name,
        Err(err) => {
            let id_value = user_id.get();
            tracing::warn!(user_id = id_value, ?err, "failed to fetch user info");
            format!("User {}", id_value)
        }
    }
}
