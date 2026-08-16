//! Request middleware that rejects missing/invalid macaroons before
//! Actix body extractors run. Per-route permission checks still happen
//! inside handlers.

use std::future::{ready, Ready};
use std::rc::Rc;

use actix_web::body::EitherBody;
use actix_web::dev::{Service, ServiceRequest, ServiceResponse, Transform};
use actix_web::web::Data;
use actix_web::{Error, HttpResponse};
use futures_util::future::LocalBoxFuture;

use crate::auth::{AuthError, INFO_READ};
use crate::routes::AppState;

pub struct RequireMacaroon;

impl<S, B> Transform<S, ServiceRequest> for RequireMacaroon
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error> + 'static,
    B: 'static,
{
    type Response = ServiceResponse<EitherBody<B>>;
    type Error = Error;
    type InitError = ();
    type Transform = RequireMacaroonMiddleware<S>;
    type Future = Ready<Result<Self::Transform, Self::InitError>>;

    fn new_transform(&self, service: S) -> Self::Future {
        ready(Ok(RequireMacaroonMiddleware {
            service: Rc::new(service),
        }))
    }
}

pub struct RequireMacaroonMiddleware<S> {
    service: Rc<S>,
}

impl<S, B> Service<ServiceRequest> for RequireMacaroonMiddleware<S>
where
    S: Service<ServiceRequest, Response = ServiceResponse<B>, Error = Error> + 'static,
    B: 'static,
{
    type Response = ServiceResponse<EitherBody<B>>;
    type Error = Error;
    type Future = LocalBoxFuture<'static, Result<Self::Response, Self::Error>>;

    actix_web::dev::forward_ready!(service);

    fn call(&self, req: ServiceRequest) -> Self::Future {
        let service = self.service.clone();

        Box::pin(async move {
            let bakery = req.app_data::<Data<AppState>>().map(|s| s.bakery.clone());

            let Some(bakery) = bakery else {
                let resp = actix_web::HttpResponse::InternalServerError().json(serde_json::json!({
                    "code": 500,
                    "message": "missing app state",
                    "details": []
                }));
                return Ok(req.into_response(resp).map_into_right_body());
            };

            // All default macaroons include info:read. Verifying it here
            // rejects missing/tampered credentials before JSON body parsing.
            if let Err(err) = crate::routes::authorize(req.request(), &bakery, INFO_READ) {
                let (status, message) = match err {
                    AuthError::Missing
                    | AuthError::Malformed
                    | AuthError::InvalidSignature
                    | AuthError::TooLarge => {
                        (actix_web::http::StatusCode::UNAUTHORIZED, err.to_string())
                    }
                    AuthError::PermissionDenied | AuthError::UnknownCaveat(_) => {
                        (actix_web::http::StatusCode::FORBIDDEN, err.to_string())
                    }
                    AuthError::Io(_) | AuthError::Other(_) => (
                        actix_web::http::StatusCode::INTERNAL_SERVER_ERROR,
                        err.to_string(),
                    ),
                };
                let resp = actix_web::HttpResponse::build(status).json(serde_json::json!({
                    "code": status.as_u16(),
                    "message": message,
                    "details": []
                }));
                return Ok(req.into_response(resp).map_into_right_body());
            }

            let res = service.call(req).await?;
            Ok(res.map_into_left_body())
        })
    }
}
