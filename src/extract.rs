use crate::{
    body::Body,
    response::{BoxIntoResponse, IntoResponse},
    Error,
};
use async_trait::async_trait;
use bytes::Bytes;
use http::{header, Request};
use serde::de::DeserializeOwned;
use std::{collections::HashMap, convert::Infallible, str::FromStr};

#[async_trait]
pub trait FromRequest<B>: Sized {
    type Rejection: IntoResponse<B>;

    async fn from_request(req: &mut Request<Body>) -> Result<Self, Self::Rejection>;
}

#[async_trait]
impl<T, B> FromRequest<B> for Option<T>
where
    T: FromRequest<B>,
{
    type Rejection = Infallible;

    async fn from_request(req: &mut Request<Body>) -> Result<Option<T>, Self::Rejection> {
        Ok(T::from_request(req).await.ok())
    }
}

macro_rules! define_rejection {
    (
        #[status = $status:ident]
        #[body = $body:expr]
        pub struct $name:ident (());
    ) => {
        #[derive(Debug)]
        pub struct $name(());

        impl IntoResponse<Body> for $name {
            fn into_response(self) -> http::Response<Body> {
                let mut res = http::Response::new(Body::from($body));
                *res.status_mut() = http::StatusCode::$status;
                res
            }
        }
    };

    (
        #[status = $status:ident]
        #[body = $body:expr]
        pub struct $name:ident (BoxError);
    ) => {
        #[derive(Debug)]
        pub struct $name(tower::BoxError);

        impl $name {
            fn from_err<E>(err: E) -> Self
            where
                E: Into<tower::BoxError>,
            {
                Self(err.into())
            }
        }

        impl IntoResponse<Body> for $name {
            fn into_response(self) -> http::Response<Body> {
                let mut res =
                    http::Response::new(Body::from(format!(concat!($body, ": {}"), self.0)));
                *res.status_mut() = http::StatusCode::$status;
                res
            }
        }
    };
}

define_rejection! {
    #[status = BAD_REQUEST]
    #[body = "Query string was invalid or missing"]
    pub struct QueryStringMissing(());
}

#[derive(Debug, Clone, Copy)]
pub struct Query<T>(T);

impl<T> Query<T> {
    pub fn into_inner(self) -> T {
        self.0
    }
}

#[async_trait]
impl<T> FromRequest<Body> for Query<T>
where
    T: DeserializeOwned,
{
    type Rejection = QueryStringMissing;

    async fn from_request(req: &mut Request<Body>) -> Result<Self, Self::Rejection> {
        let query = req.uri().query().ok_or(QueryStringMissing(()))?;
        let value = serde_urlencoded::from_str(query).map_err(|_| QueryStringMissing(()))?;
        Ok(Query(value))
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Json<T>(T);

impl<T> Json<T> {
    pub fn into_inner(self) -> T {
        self.0
    }
}

define_rejection! {
    #[status = BAD_REQUEST]
    #[body = "Failed to parse the response body as JSON"]
    pub struct InvalidJsonBody(BoxError);
}

define_rejection! {
    #[status = BAD_REQUEST]
    #[body = "Expected request with `Content-Type: application/json`"]
    pub struct MissingJsonContentType(());
}

#[async_trait]
impl<T> FromRequest<Body> for Json<T>
where
    T: DeserializeOwned,
{
    type Rejection = BoxIntoResponse<Body>;

    async fn from_request(req: &mut Request<Body>) -> Result<Self, Self::Rejection> {
        if has_content_type(&req, "application/json") {
            let body = take_body(req).map_err(IntoResponse::boxed)?;

            let bytes = hyper::body::to_bytes(body)
                .await
                .map_err(InvalidJsonBody::from_err)
                .map_err(IntoResponse::boxed)?;

            let value = serde_json::from_slice(&bytes)
                .map_err(InvalidJsonBody::from_err)
                .map_err(IntoResponse::boxed)?;

            Ok(Json(value))
        } else {
            Err(MissingJsonContentType(()).boxed())
        }
    }
}

fn has_content_type<B>(req: &Request<B>, expected_content_type: &str) -> bool {
    let content_type = if let Some(content_type) = req.headers().get(header::CONTENT_TYPE) {
        content_type
    } else {
        return false;
    };

    let content_type = if let Ok(content_type) = content_type.to_str() {
        content_type
    } else {
        return false;
    };

    content_type.starts_with(expected_content_type)
}

define_rejection! {
    #[status = INTERNAL_SERVER_ERROR]
    #[body = "Missing request extension"]
    pub struct MissingExtension(());
}

#[derive(Debug, Clone, Copy)]
pub struct Extension<T>(T);

impl<T> Extension<T> {
    pub fn into_inner(self) -> T {
        self.0
    }
}

#[async_trait]
impl<T> FromRequest<Body> for Extension<T>
where
    T: Clone + Send + Sync + 'static,
{
    type Rejection = MissingExtension;

    async fn from_request(req: &mut Request<Body>) -> Result<Self, Self::Rejection> {
        let value = req
            .extensions()
            .get::<T>()
            .ok_or(MissingExtension(()))
            .map(|x| x.clone())?;

        Ok(Extension(value))
    }
}

define_rejection! {
    #[status = BAD_REQUEST]
    #[body = "Failed to buffer the request body"]
    pub struct FailedToBufferBody(BoxError);
}

#[async_trait]
impl FromRequest<Body> for Bytes {
    type Rejection = BoxIntoResponse<Body>;

    async fn from_request(req: &mut Request<Body>) -> Result<Self, Self::Rejection> {
        let body = take_body(req).map_err(IntoResponse::boxed)?;

        let bytes = hyper::body::to_bytes(body)
            .await
            .map_err(FailedToBufferBody::from_err)
            .map_err(IntoResponse::boxed)?;

        Ok(bytes)
    }
}

define_rejection! {
    #[status = BAD_REQUEST]
    #[body = "Response body didn't contain valid UTF-8"]
    pub struct InvalidUtf8(BoxError);
}

#[async_trait]
impl FromRequest<Body> for String {
    type Rejection = BoxIntoResponse<Body>;

    async fn from_request(req: &mut Request<Body>) -> Result<Self, Self::Rejection> {
        let body = take_body(req).map_err(IntoResponse::boxed)?;

        let bytes = hyper::body::to_bytes(body)
            .await
            .map_err(FailedToBufferBody::from_err)
            .map_err(IntoResponse::boxed)?
            .to_vec();

        let string = String::from_utf8(bytes)
            .map_err(InvalidUtf8::from_err)
            .map_err(IntoResponse::boxed)?;

        Ok(string)
    }
}

#[async_trait]
impl FromRequest<Body> for Body {
    type Rejection = BodyAlreadyTaken;

    async fn from_request(req: &mut Request<Body>) -> Result<Self, Self::Rejection> {
        take_body(req)
    }
}

define_rejection! {
    #[status = PAYLOAD_TOO_LARGE]
    #[body = "Request payload is too large"]
    pub struct PayloadTooLarge(());
}

define_rejection! {
    #[status = LENGTH_REQUIRED]
    #[body = "Content length header is required"]
    pub struct LengthRequired(());
}

#[derive(Debug, Clone)]
pub struct BytesMaxLength<const N: u64>(Bytes);

impl<const N: u64> BytesMaxLength<N> {
    pub fn into_inner(self) -> Bytes {
        self.0
    }
}

#[async_trait]
impl<const N: u64> FromRequest<Body> for BytesMaxLength<N> {
    type Rejection = BoxIntoResponse<Body>;

    async fn from_request(req: &mut Request<Body>) -> Result<Self, Self::Rejection> {
        let content_length = req.headers().get(http::header::CONTENT_LENGTH).cloned();
        let body = take_body(req).map_err(|reject| reject.boxed())?;

        let content_length =
            content_length.and_then(|value| value.to_str().ok()?.parse::<u64>().ok());

        if let Some(length) = content_length {
            if length > N {
                return Err(PayloadTooLarge(()).boxed());
            }
        } else {
            return Err(LengthRequired(()).boxed());
        };

        let bytes = hyper::body::to_bytes(body)
            .await
            .map_err(|e| FailedToBufferBody::from_err(e).boxed())?;

        Ok(BytesMaxLength(bytes))
    }
}

define_rejection! {
    #[status = INTERNAL_SERVER_ERROR]
    #[body = "No url params found for matched route. This is a bug in tower-web. Please open an issue"]
    pub struct MissingRouteParams(());
}

pub struct UrlParamsMap(HashMap<String, String>);

impl UrlParamsMap {
    pub fn get(&self, key: &str) -> Result<&str, Error> {
        if let Some(value) = self.0.get(key) {
            Ok(value)
        } else {
            Err(Error::UnknownUrlParam(key.to_string()))
        }
    }

    pub fn get_typed<T>(&self, key: &str) -> Result<T, Error>
    where
        T: FromStr,
    {
        self.get(key)?.parse().map_err(|_| Error::InvalidUrlParam {
            type_name: std::any::type_name::<T>(),
        })
    }
}

#[async_trait]
impl FromRequest<Body> for UrlParamsMap {
    type Rejection = MissingRouteParams;

    async fn from_request(req: &mut Request<Body>) -> Result<Self, Self::Rejection> {
        if let Some(params) = req
            .extensions_mut()
            .get_mut::<Option<crate::routing::UrlParams>>()
        {
            let params = params.take().expect("params already taken").0;
            Ok(Self(params.into_iter().collect()))
        } else {
            Err(MissingRouteParams(()))
        }
    }
}

#[derive(Debug)]
pub struct InvalidUrlParam {
    type_name: &'static str,
}

impl InvalidUrlParam {
    fn new<T>() -> Self {
        InvalidUrlParam {
            type_name: std::any::type_name::<T>(),
        }
    }
}

impl IntoResponse<Body> for InvalidUrlParam {
    fn into_response(self) -> http::Response<Body> {
        let mut res = http::Response::new(Body::from(format!(
            "Invalid URL param. Expected something of type `{}`",
            self.type_name
        )));
        *res.status_mut() = http::StatusCode::BAD_REQUEST;
        res
    }
}

pub struct UrlParams<T>(T);

macro_rules! impl_parse_url {
    () => {};

    ( $head:ident, $($tail:ident),* $(,)? ) => {
        #[async_trait]
        impl<$head, $($tail,)*> FromRequest<Body> for UrlParams<($head, $($tail,)*)>
        where
            $head: FromStr + Send,
            $( $tail: FromStr + Send, )*
        {
            type Rejection = BoxIntoResponse<Body>;

            #[allow(non_snake_case)]
            async fn from_request(req: &mut Request<Body>) -> Result<Self, Self::Rejection> {
                let params = if let Some(params) = req
                    .extensions_mut()
                    .get_mut::<Option<crate::routing::UrlParams>>()
                {
                    params.take().expect("params already taken").0
                } else {
                    return Err(MissingRouteParams(()).boxed())
                };

                if let [(_, $head), $((_, $tail),)*] = &*params {
                    let $head = if let Ok(x) = $head.parse::<$head>() {
                       x
                    } else {
                        return Err(InvalidUrlParam::new::<$head>().boxed());
                    };

                    $(
                        let $tail = if let Ok(x) = $tail.parse::<$tail>() {
                           x
                        } else {
                            return Err(InvalidUrlParam::new::<$tail>().boxed());
                        };
                    )*

                    Ok(UrlParams(($head, $($tail,)*)))
                } else {
                    return Err(MissingRouteParams(()).boxed())
                }
            }
        }

        impl_parse_url!($($tail,)*);
    };
}

impl_parse_url!(T1, T2, T3, T4, T5, T6);

impl<T1> UrlParams<(T1,)> {
    pub fn into_inner(self) -> T1 {
        (self.0).0
    }
}

impl<T1, T2> UrlParams<(T1, T2)> {
    pub fn into_inner(self) -> (T1, T2) {
        ((self.0).0, (self.0).1)
    }
}

impl<T1, T2, T3> UrlParams<(T1, T2, T3)> {
    pub fn into_inner(self) -> (T1, T2, T3) {
        ((self.0).0, (self.0).1, (self.0).2)
    }
}

impl<T1, T2, T3, T4> UrlParams<(T1, T2, T3, T4)> {
    pub fn into_inner(self) -> (T1, T2, T3, T4) {
        ((self.0).0, (self.0).1, (self.0).2, (self.0).3)
    }
}

impl<T1, T2, T3, T4, T5> UrlParams<(T1, T2, T3, T4, T5)> {
    pub fn into_inner(self) -> (T1, T2, T3, T4, T5) {
        ((self.0).0, (self.0).1, (self.0).2, (self.0).3, (self.0).4)
    }
}

impl<T1, T2, T3, T4, T5, T6> UrlParams<(T1, T2, T3, T4, T5, T6)> {
    pub fn into_inner(self) -> (T1, T2, T3, T4, T5, T6) {
        (
            (self.0).0,
            (self.0).1,
            (self.0).2,
            (self.0).3,
            (self.0).4,
            (self.0).5,
        )
    }
}

define_rejection! {
    #[status = INTERNAL_SERVER_ERROR]
    #[body = "Cannot have two request body extractors for a single handler"]
    pub struct BodyAlreadyTaken(());
}

fn take_body(req: &mut Request<Body>) -> Result<Body, BodyAlreadyTaken> {
    struct BodyAlreadyTakenExt;

    if req.extensions_mut().insert(BodyAlreadyTakenExt).is_some() {
        Err(BodyAlreadyTaken(()))
    } else {
        let body = std::mem::take(req.body_mut());
        Ok(body)
    }
}
