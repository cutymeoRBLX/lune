use std::collections::HashMap;

use mlua::prelude::*;

use hyper::header::{CONTENT_ENCODING, CONTENT_LENGTH};

use crate::lune::{scheduler::Scheduler, util::TableBuilder};

use self::server::create_server;

use super::serde::{
    compress_decompress::{decompress, CompressDecompressFormat},
    encode_decode::{EncodeDecodeConfig, EncodeDecodeFormat},
};

mod client;
mod config;
mod processing;
mod response;
mod server;
mod websocket;

use client::{NetClient, NetClientBuilder};
use config::{RequestConfig, ServeConfig};
use server::bind_to_localhost;
use websocket::NetWebSocket;

pub fn create(lua: &'static Lua) -> LuaResult<LuaTable> {
    NetClientBuilder::new()
        .headers(&[("User-Agent", create_user_agent_header())])?
        .build()?
        .into_registry(lua);
    TableBuilder::new(lua)?
        .with_function("jsonEncode", net_json_encode)?
        .with_function("jsonDecode", net_json_decode)?
        .with_async_function("request", net_request)?
        .with_async_function("socket", net_socket)?
        .with_async_function("serve", net_serve)?
        .with_function("urlEncode", net_url_encode)?
        .with_function("urlDecode", net_url_decode)?
        .build_readonly()
}

fn create_user_agent_header() -> String {
    let (github_owner, github_repo) = env!("CARGO_PKG_REPOSITORY")
        .trim_start_matches("https://github.com/")
        .split_once('/')
        .unwrap();
    format!("{github_owner}-{github_repo}-cli")
}

fn net_json_encode<'lua>(
    lua: &'lua Lua,
    (val, pretty): (LuaValue<'lua>, Option<bool>),
) -> LuaResult<LuaString<'lua>> {
    EncodeDecodeConfig::from((EncodeDecodeFormat::Json, pretty.unwrap_or_default()))
        .serialize_to_string(lua, val)
}

fn net_json_decode<'lua>(lua: &'lua Lua, json: LuaString<'lua>) -> LuaResult<LuaValue<'lua>> {
    EncodeDecodeConfig::from(EncodeDecodeFormat::Json).deserialize_from_string(lua, json)
}

async fn net_request<'lua>(lua: &'lua Lua, config: RequestConfig<'lua>) -> LuaResult<LuaTable<'lua>>
where
    'lua: 'static, // FIXME: Get rid of static lifetime bound here
{
    // Create and send the request
    let client = NetClient::from_registry(lua);
    let mut request = client.request(config.method, &config.url);
    for (query, value) in config.query {
        request = request.query(&[(query.to_str()?, value.to_str()?)]);
    }
    for (header, value) in config.headers {
        request = request.header(header.to_str()?, value.to_str()?);
    }
    let res = request
        .body(config.body.unwrap_or_default())
        .send()
        .await
        .into_lua_err()?;
    // Extract status, headers
    let res_status = res.status().as_u16();
    let res_status_text = res.status().canonical_reason();
    let mut res_headers = res
        .headers()
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_string(),
                value.to_str().unwrap().to_owned(),
            )
        })
        .collect::<HashMap<String, String>>();
    // Read response bytes
    let mut res_bytes = res.bytes().await.into_lua_err()?.to_vec();
    // Check for extra options, decompression
    if config.options.decompress {
        // NOTE: Header names are guaranteed to be lowercase because of the above
        // transformations of them into the hashmap, so we can compare directly
        let format = res_headers.iter().find_map(|(name, val)| {
            if name == CONTENT_ENCODING.as_str() {
                CompressDecompressFormat::detect_from_header_str(val)
            } else {
                None
            }
        });
        if let Some(format) = format {
            res_bytes = decompress(format, res_bytes).await?;
            let content_encoding_header_str = CONTENT_ENCODING.as_str();
            let content_length_header_str = CONTENT_LENGTH.as_str();
            res_headers.retain(|name, _| {
                name != content_encoding_header_str && name != content_length_header_str
            });
        }
    }
    // Construct and return a readonly lua table with results
    TableBuilder::new(lua)?
        .with_value("ok", (200..300).contains(&res_status))?
        .with_value("statusCode", res_status)?
        .with_value("statusMessage", res_status_text)?
        .with_value("headers", res_headers)?
        .with_value("body", lua.create_string(&res_bytes)?)?
        .build_readonly()
}

async fn net_socket<'lua>(lua: &'lua Lua, url: String) -> LuaResult<LuaTable>
where
    'lua: 'static, // FIXME: Get rid of static lifetime bound here
{
    let (ws, _) = tokio_tungstenite::connect_async(url).await.into_lua_err()?;
    NetWebSocket::new(ws).into_lua_table(lua)
}

async fn net_serve<'lua>(
    lua: &'lua Lua,
    (port, config): (u16, ServeConfig<'lua>),
) -> LuaResult<LuaTable<'lua>>
where
    'lua: 'static, // FIXME: Get rid of static lifetime bound here
{
    let sched = lua
        .app_data_ref::<&Scheduler>()
        .expect("Lua struct is missing scheduler");

    let builder = bind_to_localhost(port)?;

    create_server(lua, &sched, config, builder)
}

fn net_url_encode<'lua>(
    lua: &'lua Lua,
    (lua_string, as_binary): (LuaString<'lua>, Option<bool>),
) -> LuaResult<LuaValue<'lua>> {
    if matches!(as_binary, Some(true)) {
        urlencoding::encode_binary(lua_string.as_bytes()).into_lua(lua)
    } else {
        urlencoding::encode(lua_string.to_str()?).into_lua(lua)
    }
}

fn net_url_decode<'lua>(
    lua: &'lua Lua,
    (lua_string, as_binary): (LuaString<'lua>, Option<bool>),
) -> LuaResult<LuaValue<'lua>> {
    if matches!(as_binary, Some(true)) {
        urlencoding::decode_binary(lua_string.as_bytes()).into_lua(lua)
    } else {
        urlencoding::decode(lua_string.to_str()?)
            .map_err(|e| LuaError::RuntimeError(format!("Encountered invalid encoding - {e}")))?
            .into_lua(lua)
    }
}
