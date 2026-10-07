//! Serve the contract derived from the production route annotations and serde DTOs.

use axum::{Router, response::Html, routing::get};

pub const OPENAPI: &[u8] = include_bytes!("../../../web/openapi.json");

pub fn routes() -> Router {
    Router::new()
        .route(
            "/openapi.json",
            get(|| async { axum::Json(crate::generated_openapi()) }),
        )
        .route("/docs", get(swagger))
        .route("/docs/oauth2-redirect", get(oauth_redirect))
        .route("/redoc", get(redoc))
}

async fn swagger() -> Html<&'static str> {
    Html(
        r#"<!doctype html><html><head><title>Cannery Row - Swagger UI</title><link rel="stylesheet" href="https://cdn.jsdelivr.net/npm/swagger-ui-dist@5/swagger-ui.css"></head><body><div id="swagger-ui"></div><script src="https://cdn.jsdelivr.net/npm/swagger-ui-dist@5/swagger-ui-bundle.js"></script><script>SwaggerUIBundle({url:'/openapi.json',dom_id:'#swagger-ui',deepLinking:true,displayRequestDuration:true,oauth2RedirectUrl:window.location.origin+'/docs/oauth2-redirect',presets:[SwaggerUIBundle.presets.apis,SwaggerUIBundle.SwaggerUIStandalonePreset]});</script></body></html>"#,
    )
}

async fn oauth_redirect() -> Html<&'static str> {
    // Preserve the Swagger redirect page shipped by the frozen FastAPI dependency.
    Html(include_str!("oauth2-redirect.html"))
}

async fn redoc() -> Html<&'static str> {
    Html(
        r#"<!doctype html><html><head><title>Cannery Row - ReDoc</title><meta name="viewport" content="width=device-width, initial-scale=1"><style>body{margin:0;padding:0}</style></head><body><redoc spec-url="/openapi.json"></redoc><script src="https://cdn.jsdelivr.net/npm/redoc@2/bundles/redoc.standalone.js"></script></body></html>"#,
    )
}
