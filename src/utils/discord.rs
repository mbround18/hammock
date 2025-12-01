use serenity::{model::id::UserId, prelude::Context};

pub async fn resolve_user_name(ctx: &Context, user_id: UserId) -> String {
    if let Some(user) = ctx.cache.user(user_id) {
        return user.name.clone();
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
