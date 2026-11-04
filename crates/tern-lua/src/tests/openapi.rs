//! T4c: connectors generated from OpenAPI documents, and recorded fixtures.

use serde_json::{Value, json};
use tern_core::tool::{Effect, Tool};
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::{ctx, host_with};
use crate::fixtures::{self, Fixtures};
use crate::openapi::{self, Scheme};
use crate::tools::{ConnectorActivate, ConnectorFromOpenapi, ConnectorTest, Dirs};
use crate::{Inject, Vm};

const PETSTORE_YAML: &str = r#"
openapi: 3.0.3
info:
  title: Petstore
  version: 1.0.0
  x-rate-limit: 30/min
servers:
  - url: http://staging.petstore.example.com/v1
  - url: https://petstore.example.com/v1
security:
  - api_key: []
paths:
  /pets:
    get:
      operationId: listPets
      summary: List all pets
      tags: [pets]
      parameters:
        - name: limit
          in: query
          description: How many items to return
          schema: { type: integer, maximum: 100 }
        - name: tags
          in: query
          schema: { type: array, items: { type: string } }
    post:
      operationId: createPets
      summary: Create a pet
      tags: [pets]
      requestBody:
        required: true
        content:
          application/json:
            schema: { $ref: '#/components/schemas/NewPet' }
  /pets/{petId}:
    parameters:
      - name: petId
        in: path
        required: true
        schema: { type: string }
    get:
      operationId: showPetById
      summary: Info for a specific pet
      tags: [pets]
    delete:
      operationId: deletePet
      summary: Delete a pet
      tags: [pets]
  /store/inventory:
    get:
      operationId: getInventory
      deprecated: true
      tags: [store]
components:
  securitySchemes:
    api_key:
      type: apiKey
      in: header
      name: X-API-Key
  schemas:
    NewPet:
      type: object
      required: [name]
      properties:
        id: { type: integer, readOnly: true }
        name: { type: string }
        owner: { $ref: '#/components/schemas/Owner' }
    Owner:
      allOf:
        - type: object
          properties: { name: { type: string } }
        - type: object
          required: [email]
          properties: { email: { type: string, format: email } }
"#;

fn all(doc: &openapi::Document) -> Vec<usize> {
    (0..doc.operations.len()).collect()
}

fn load_generated(src: &str) -> Vm {
    let (host, _tmp) = host_with(&[]);
    match Vm::load(&host, "gen.luau", src) {
        Ok(vm) => vm,
        Err(e) => panic!("generated connector didn't load: {e}\n{src}"),
    }
}

fn effect_of(vm: &Vm, tool: &str) -> Effect {
    vm.spec
        .tools
        .iter()
        .find(|t| t.name == tool)
        .unwrap_or_else(|| panic!("no tool {tool}"))
        .effect()
}

#[test]
fn openapi3_yaml_effects_allowlist_auth_and_rate() {
    let doc = openapi::parse(PETSTORE_YAML, None, None).unwrap();
    assert_eq!(doc.version, "3.0.3");
    // The https server wins over the first-listed http one.
    assert_eq!(doc.base_url, "https://petstore.example.com/v1");
    assert_eq!(doc.rate.map(|r| r.to_string()).as_deref(), Some("30/min"));
    assert_eq!(doc.suggested_name(), "petstore");
    let keys: Vec<&str> = doc.operations.iter().map(|o| o.key.as_str()).collect();
    assert_eq!(
        keys,
        [
            "list_pets",
            "create_pets",
            "show_pet_by_id",
            "delete_pet",
            "get_inventory"
        ]
    );

    // Default: non-deprecated reads only.
    let default = openapi::select(&doc, None, 8, 40).unwrap();
    let names: Vec<&str> = default
        .iter()
        .map(|&i| doc.operations[i].key.as_str())
        .collect();
    assert_eq!(names, ["list_pets", "show_pet_by_id"]);

    let g = openapi::generate(&doc, "petstore", &all(&doc), "https://x/openapi.yaml").unwrap();
    let vm = load_generated(&g.source);
    assert_eq!(effect_of(&vm, "list_pets"), Effect::Read);
    assert_eq!(effect_of(&vm, "show_pet_by_id"), Effect::Read);
    assert_eq!(effect_of(&vm, "create_pets"), Effect::Write);
    assert_eq!(effect_of(&vm, "delete_pet"), Effect::Destructive);
    let list = vm
        .spec
        .tools
        .iter()
        .find(|t| t.name == "list_pets")
        .unwrap();
    assert!(list.untrusted && list.idempotent);
    let caps = &vm.spec.capabilities;
    assert_eq!(
        caps.http,
        [
            "petstore.example.com/v1/pets",
            "petstore.example.com/v1/pets/*",
            "petstore.example.com/v1/store/inventory"
        ]
    );
    assert_eq!(caps.rate.map(|r| r.to_string()).as_deref(), Some("30/min"));
    assert_eq!(caps.auth.len(), 1);
    assert_eq!(
        caps.auth[0].inject,
        Inject::Header {
            header: "X-API-Key".into(),
            scheme: String::new(),
            secret: "PETSTORE_API_KEY".into()
        }
    );
    assert!(caps.secrets.is_empty(), "never the legacy ctx.secret");
    assert_eq!(g.secrets, ["PETSTORE_API_KEY"]);

    // Typed params: query types kept, the body's $refs and allOf resolved,
    // readOnly fields left out, path params required.
    assert_eq!(list.params["properties"]["limit"]["type"], "integer");
    assert_eq!(list.params["properties"]["tags"]["items"]["type"], "string");
    let create = vm
        .spec
        .tools
        .iter()
        .find(|t| t.name == "create_pets")
        .unwrap();
    let body = &create.params["properties"]["body"];
    assert_eq!(create.params["required"], json!(["body"]));
    assert_eq!(body["required"], json!(["name"]));
    assert!(body["properties"].get("id").is_none());
    assert_eq!(body["properties"]["owner"]["required"], json!(["email"]));
    let show = vm
        .spec
        .tools
        .iter()
        .find(|t| t.name == "show_pet_by_id")
        .unwrap();
    assert_eq!(show.params["required"], json!(["petId"]));
}

#[test]
fn oauth2_schemes_feed_the_oauth_first_flow() {
    let doc = json!({
        "openapi": "3.1.0",
        "info": { "title": "Acme Widgets", "version": "1" },
        "servers": [{ "url": "https://api.acmewidgets.example/{version}", "variables": { "version": { "default": "v2" } } }],
        "components": { "securitySchemes": {
            "oauth": { "type": "oauth2", "flows": {
                "clientCredentials": { "tokenUrl": "https://auth.acmewidgets.example/cc", "scopes": {} },
                "authorizationCode": {
                    "authorizationUrl": "https://auth.acmewidgets.example/authorize",
                    "tokenUrl": "https://auth.acmewidgets.example/token",
                    "scopes": { "widgets:read": "read", "widgets:write": "write", "admin": "all" }
                }
            }},
            "key": { "type": "apiKey", "in": "query", "name": "api_key" }
        }},
        "paths": {
            "/widgets": {
                "get": { "operationId": "listWidgets", "security": [{ "oauth": ["widgets:read"] }, { "key": [] }] },
                "post": { "operationId": "makeWidget", "security": [{ "oauth": ["widgets:write"] }] }
            }
        }
    });
    let doc = openapi::parse(&doc.to_string(), None, None).unwrap();
    assert_eq!(doc.base_url, "https://api.acmewidgets.example/v2");
    assert!(matches!(doc.schemes["oauth"], Scheme::OAuth2 { .. }));
    let g = openapi::generate(
        &doc,
        "acme",
        &all(&doc),
        "https://acmewidgets.example/openapi.json",
    )
    .unwrap();
    let vm = load_generated(&g.source);
    let oauth = vm
        .spec
        .capabilities
        .oauth
        .clone()
        .expect("capabilities.oauth");
    // Named by the whole registrable domain (T4a security fixes), so a
    // lookalike can't borrow a configured client.
    assert_eq!(oauth.provider, "acmewidgets_example");
    assert_eq!(
        oauth.scopes,
        ["widgets:read", "widgets:write"],
        "only what the tools need"
    );
    // No `auth` line: the account's token is sent by tern as a bearer.
    assert!(!g.source.contains("    auth ="), "{}", g.source);
    assert_eq!(vm.spec.capabilities.auth_secrets(), ["oauth:acmewidgets_example"]);
    let plan = g.oauth_plan.expect("a plan for auth_propose");
    assert_eq!(
        plan["authorize_url"],
        "https://auth.acmewidgets.example/authorize"
    );
    assert_eq!(plan["token_url"], "https://auth.acmewidgets.example/token");
    assert_eq!(plan["api_hosts"], json!(["api.acmewidgets.example"]));
    assert_eq!(
        plan["evidence"],
        json!(["https://acmewidgets.example/openapi.json"])
    );
    assert!(
        g.notes.iter().any(|n| n.contains("auth_discover")),
        "{:?}",
        g.notes
    );

    // A service tern has a built-in profile for uses it, with no plan.
    let gh = json!({
        "openapi": "3.0.0",
        "info": { "title": "GitHub v3 REST API", "version": "1" },
        "servers": [{ "url": "https://api.github.com" }],
        "security": [{ "oauth": ["repo"] }],
        "components": { "securitySchemes": { "oauth": { "type": "oauth2", "flows": { "authorizationCode": {
            "authorizationUrl": "https://github.com/login/oauth/authorize",
            "tokenUrl": "https://github.com/login/oauth/access_token", "scopes": { "repo": "" } } } } } },
        "paths": { "/user/repos": { "get": { "operationId": "repos/list-for-authenticated-user" } } }
    });
    let gh = openapi::parse(&gh.to_string(), None, None).unwrap();
    let g = openapi::generate(&gh, "github", &all(&gh), "").unwrap();
    let vm = load_generated(&g.source);
    assert_eq!(
        vm.spec.capabilities.oauth.as_ref().unwrap().provider,
        "github"
    );
    assert!(g.oauth_plan.is_none());
    assert_eq!(vm.spec.tools[0].name, "repos_list_for_authenticated_use");
}

#[test]
fn swagger2_base_body_form_and_query_key() {
    let doc = json!({
        "swagger": "2.0",
        "info": { "title": "County Assessor", "version": "2", "description": "Parcel data. Limited to 10 requests per second." },
        "host": "data.county.example.gov",
        "basePath": "/assessor/v2",
        "schemes": ["http", "https"],
        "securityDefinitions": {
            "key": { "type": "apiKey", "in": "query", "name": "token" },
            "basic": { "type": "basic" }
        },
        "security": [{ "key": [] }],
        "paths": {
            "/parcels": {
                "get": { "operationId": "searchParcels", "parameters": [
                    { "name": "owner", "in": "query", "type": "string" },
                    { "name": "ids", "in": "query", "type": "array", "items": { "type": "string" }, "collectionFormat": "multi" }
                ]}
            },
            "/parcels/{parcel_id}": {
                "put": { "operationId": "updateParcel", "parameters": [
                    { "name": "parcel_id", "in": "path", "required": true, "type": "string" },
                    { "name": "body", "in": "body", "required": true, "schema": { "$ref": "#/definitions/Parcel" } }
                ]}
            },
            "/appeals": {
                "post": { "operationId": "fileAppeal", "security": [{ "basic": [] }], "parameters": [
                    { "name": "parcel_id", "in": "formData", "type": "string", "required": true },
                    { "name": "reason", "in": "formData", "type": "string" }
                ]}
            },
            "/photos": {
                "post": { "operationId": "uploadPhoto", "parameters": [
                    { "name": "file", "in": "formData", "type": "file", "required": true }
                ]}
            }
        },
        "definitions": { "Parcel": { "type": "object", "properties": { "value": { "type": "number" } } } }
    });
    let doc = openapi::parse(&doc.to_string(), None, None).unwrap();
    assert_eq!(doc.version, "2.0");
    assert_eq!(doc.base_url, "https://data.county.example.gov/assessor/v2");
    assert_eq!(doc.rate.map(|r| r.to_string()).as_deref(), Some("10/sec"));
    let upload = doc
        .operations
        .iter()
        .find(|o| o.key == "upload_photo")
        .unwrap();
    assert!(
        upload.unsupported.is_some(),
        "file uploads can't be generated"
    );
    let err = openapi::select(&doc, Some(&["upload_photo".into()]), 8, 40).unwrap_err();
    assert!(err.contains("file uploads"), "{err}");

    let chosen = openapi::select(
        &doc,
        Some(&[
            "searchParcels".into(),
            "PUT /parcels/{parcel_id}".into(),
            "file_appeal".into(),
        ]),
        8,
        40,
    )
    .unwrap();
    let g = openapi::generate(&doc, "assessor", &chosen, "").unwrap();
    let vm = load_generated(&g.source);
    assert_eq!(effect_of(&vm, "search_parcels"), Effect::Read);
    assert_eq!(effect_of(&vm, "update_parcel"), Effect::Write);
    assert!(
        vm.spec
            .tools
            .iter()
            .find(|t| t.name == "update_parcel")
            .unwrap()
            .idempotent
    );
    assert_eq!(effect_of(&vm, "file_appeal"), Effect::Write);
    // The query key is the best scheme the chosen operations use.
    assert_eq!(
        vm.spec.capabilities.auth[0].inject,
        Inject::Query {
            param: "token".into(),
            secret: "ASSESSOR_API_KEY".into()
        }
    );
    assert_eq!(
        vm.spec.capabilities.http,
        [
            "data.county.example.gov/assessor/v2/appeals",
            "data.county.example.gov/assessor/v2/parcels",
            "data.county.example.gov/assessor/v2/parcels/*"
        ]
    );
    let update = vm
        .spec
        .tools
        .iter()
        .find(|t| t.name == "update_parcel")
        .unwrap();
    assert_eq!(
        update.params["properties"]["body"]["properties"]["value"]["type"],
        "number"
    );
    let appeal = vm
        .spec
        .tools
        .iter()
        .find(|t| t.name == "file_appeal")
        .unwrap();
    assert_eq!(
        appeal.params["properties"]["body"]["required"],
        json!(["parcel_id"])
    );

    // A relative server needs somewhere to resolve against.
    let rel = r#"{"openapi":"3.0.0","info":{"title":"x","version":"1"},"servers":[{"url":"/api"}],"paths":{"/a":{"get":{}}}}"#;
    assert!(
        openapi::parse(rel, None, None)
            .unwrap_err()
            .contains("base_url")
    );
    let ok = openapi::parse(rel, Some("https://docs.x.example/spec.json"), None).unwrap();
    assert_eq!(ok.base_url, "https://docs.x.example/api");
    assert_eq!(ok.operations[0].key, "get_a");
}

fn big_spec(n: usize) -> String {
    let mut paths = serde_json::Map::new();
    for i in 0..n {
        let method = if i % 3 == 0 { "post" } else { "get" };
        paths.insert(
            format!("/things{i:03}/{{id}}"),
            json!({ method: { "operationId": format!("op{i:03}"), "tags": [format!("tag{}", i % 10)], "summary": format!("Thing {i}"),
                "parameters": [{ "name": "id", "in": "path", "required": true, "schema": { "type": "string" } }] } }),
        );
    }
    json!({ "openapi": "3.0.0", "info": { "title": "Huge", "version": "1" },
            "servers": [{ "url": "https://huge.example.com" }], "paths": paths })
    .to_string()
}

#[tokio::test]
async fn a_500_operation_spec_is_listed_and_chosen_not_generated_whole() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/openapi.json"))
        .respond_with(ResponseTemplate::new(200).set_body_string(big_spec(500)))
        .expect(1) // fetched once, then reused
        .mount(&server)
        .await;
    let (host, tmp) = host_with(&[]);
    let dirs = Dirs {
        active: tmp.path().join("c"),
        drafts: tmp.path().join("c/drafts"),
    };
    let tool = ConnectorFromOpenapi::new(host, dirs.clone()).allowing_loopback();
    let url = format!("{}/openapi.json", server.uri());
    let c = ctx();

    let r = tool.call(&c, json!({ "url": url })).await;
    assert!(!r.is_error, "{}", r.content);
    assert!(r.content.contains("500 operations"), "{}", r.content);
    assert!(
        r.content.contains("too large to generate whole"),
        "{}",
        r.content
    );
    assert!(r.content.contains("(more: offset 50)"), "{}", r.content);
    assert!(r.content.contains("tag0 (50)"), "{}", r.content);
    assert!(!dirs.draft_path("huge").exists(), "nothing written");

    let page = tool
        .call(
            &c,
            json!({ "url": url, "list": true, "tag": "tag3", "offset": 40 }),
        )
        .await;
    assert!(
        page.content
            .contains("Operations matching tag \"tag3\" 41–50 of 50"),
        "{}",
        page.content
    );
    assert_eq!(page.content.matches("\n- ").count(), 10);
    let found = tool
        .call(
            &c,
            json!({ "url": url, "list": true, "search": "thing 42" }),
        )
        .await;
    assert!(found.content.contains("op042"), "{}", found.content);

    let too_many = tool
        .call(&c, json!({ "url": url, "operations": ["tag:tag3"] }))
        .await;
    assert!(
        too_many.is_error && too_many.content.contains("at most 40"),
        "{}",
        too_many.content
    );
    let unknown = tool
        .call(&c, json!({ "url": url, "operations": ["nope"] }))
        .await;
    assert!(
        unknown.is_error && unknown.content.contains("nope"),
        "{}",
        unknown.content
    );

    let made = tool
        .call(
            &c,
            json!({ "url": url, "operations": ["op001", "op002", "GET /things004/{id}", "op003"], "name": "huge" }),
        )
        .await;
    assert!(!made.is_error, "{}", made.content);
    assert!(
        made.content.contains("4 of 500 operations generated"),
        "{}",
        made.content
    );
    assert!(
        made.content.contains("connector_activate"),
        "{}",
        made.content
    );
    let src = std::fs::read_to_string(dirs.draft_path("huge")).unwrap();
    let vm = load_generated(&src);
    assert_eq!(vm.spec.tools.len(), 4);
    assert_eq!(effect_of(&vm, "op003"), Effect::Write);
    assert_eq!(vm.spec.capabilities.http.len(), 4);
    // Never activated by generating.
    assert!(!dirs.active.join("huge.luau").exists());
}

const LIVE_SPEC: &str = r#"{
  "openapi": "3.0.0",
  "info": { "title": "Pets", "version": "1" },
  "servers": [{ "url": "SERVER/api" }],
  "security": [{ "key": [] }],
  "components": { "securitySchemes": { "key": { "type": "apiKey", "in": "header", "name": "X-API-Key" } } },
  "paths": {
    "/pets/{petId}": { "get": { "operationId": "getPet", "parameters": [
      { "name": "petId", "in": "path", "required": true, "schema": { "type": "string" } },
      { "name": "limit", "in": "query", "schema": { "type": "integer" } },
      { "name": "tags", "in": "query", "schema": { "type": "array", "items": { "type": "string" } } }
    ] } },
    "/pets": { "post": { "operationId": "addPet", "requestBody": { "required": true,
      "content": { "application/json": { "schema": { "type": "object", "properties": { "name": { "type": "string" } } } } } } } }
  }
}"#;

#[tokio::test]
async fn a_generated_connector_passes_the_harness_and_records_and_replays_redacted() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/pets/42"))
        .and(query_param("limit", "2"))
        .and(header("x-api-key", "s3cr3t-key"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("set-cookie", "session=abc123")
                .insert_header("x-ratelimit-remaining", "59")
                .set_body_json(json!({
                    "id": 42, "name": "Rex",
                    "echo": "you sent s3cr3t-key",
                    "access_token": "leaky-token-value",
                    "next_page_token": "p2"
                })),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/pets"))
        .and(wiremock::matchers::body_json(json!({ "name": "Tom" })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({ "id": 7, "name": "Tom" })))
        .expect(1)
        .mount(&server)
        .await;

    let (host, tmp) = host_with(&[("PETS_API_KEY", "s3cr3t-key")]);
    let dirs = Dirs {
        active: tmp.path().join("c"),
        drafts: tmp.path().join("c/drafts"),
    };
    let spec = LIVE_SPEC.replace("SERVER", &server.uri());
    let doc = openapi::parse(&spec, None, None).unwrap();
    let chosen = openapi::select(&doc, Some(&["getPet".into(), "addPet".into()]), 8, 40).unwrap();
    crate::tools::draft_from_openapi(&host, &dirs, &doc, "pets", &chosen, "").unwrap();

    let test = ConnectorTest {
        host: host.clone(),
        dirs: dirs.clone(),
        inspected: Default::default(),
    };
    let c = ctx();
    let args = json!({ "name": "pets", "tool": "get_pet", "args": { "petId": "42", "limit": 2 } });

    // Live the first time: it reaches the network, so it asks.
    assert!(test.policy(&c, &args).requires_approval);
    let live = test.call(&c, args.clone()).await;
    assert!(!live.is_error, "{}", live.content);
    assert!(live.content.contains("\"Rex\""), "{}", live.content);
    assert!(
        !live.content.contains("s3cr3t-key"),
        "injected values are scrubbed: {}",
        live.content
    );
    assert!(
        live.content.contains("recorded 1 HTTP exchange"),
        "{}",
        live.content
    );

    let file = dirs.draft_fixtures("pets");
    assert_eq!(file, tmp.path().join("c/drafts/pets.fixtures.json"));
    let saved = std::fs::read_to_string(&file).unwrap();
    for secret in [
        "s3cr3t-key",
        "leaky-token-value",
        "abc123",
        "x-api-key",
        "X-API-Key",
    ] {
        assert!(!saved.contains(secret), "{secret} leaked into {saved}");
    }
    let f = Fixtures::load(&file).unwrap().unwrap();
    let ex = &f.cases[0].exchanges[0];
    assert_eq!(ex.request.method, "GET");
    assert!(
        ex.request.url.ends_with("/api/pets/42?limit=2"),
        "{}",
        ex.request.url
    );
    assert_eq!(
        ex.response
            .headers
            .get("x-ratelimit-remaining")
            .map(String::as_str),
        Some("59")
    );
    assert!(!ex.response.headers.contains_key("set-cookie"));
    let body: Value = serde_json::from_str(&ex.response.body).unwrap();
    assert_eq!(body["access_token"], fixtures::REDACTED);
    assert_eq!(body["next_page_token"], "p2");
    assert_eq!(body["echo"], "you sent [REDACTED]");

    // The same call again replays offline: no approval, no request (the
    // mock expects exactly one), the recorded result.
    assert!(!test.policy(&c, &args).requires_approval);
    let again = test.call(&c, args.clone()).await;
    assert!(!again.is_error, "{}", again.content);
    assert!(
        again.content.starts_with("[replayed offline"),
        "{}",
        again.content
    );
    assert!(again.content.contains("\"Rex\""), "{}", again.content);

    // Other arguments haven't been recorded: live, so it asks; and
    // replay-only says there is nothing to replay.
    let other = json!({ "name": "pets", "tool": "get_pet", "args": { "petId": "43" } });
    assert!(test.policy(&c, &other).requires_approval);
    let mut only = other.clone();
    only["mode"] = "replay".into();
    assert!(!test.policy(&c, &only).requires_approval);
    let none = test.call(&c, only).await;
    assert!(
        none.is_error && none.content.contains("no recorded call"),
        "{}",
        none.content
    );

    // A write is recorded too, and replays without sending.
    let add = json!({ "name": "pets", "tool": "add_pet", "args": { "body": { "name": "Tom" } } });
    let p = test.policy(&c, &add);
    assert!(p.requires_approval && p.effect == Effect::Write);
    let made = test.call(&c, add.clone()).await;
    assert!(
        !made.is_error && made.content.contains("\"Tom\""),
        "{}",
        made.content
    );
    assert!(!test.policy(&c, &add).requires_approval);
    let replayed = test.call(&c, add).await;
    assert!(
        replayed.content.starts_with("[replayed offline"),
        "{}",
        replayed.content
    );

    // Replays need neither the server nor the secret.
    drop(server);
    let (bare, _t) = host_with(&[]);
    let vm = crate::load_file(&bare, &dirs.draft_path("pets")).unwrap();
    let f = Fixtures::load(&file).unwrap().unwrap();
    assert_eq!(f.cases.len(), 2);
    for case in &f.cases {
        assert_eq!(
            fixtures::check_case(&vm, case).await,
            fixtures::Outcome::Pass,
            "{}",
            case.tool
        );
    }
    // A changed request isn't silently answered.
    let mut changed = f.cases[0].clone();
    changed.args["limit"] = 3.into();
    match fixtures::check_case(&vm, &changed).await {
        fixtures::Outcome::Failed(e) => assert!(e.contains("no recorded response"), "{e}"),
        other => panic!("{other:?}"),
    }

    // Activating takes the fixtures along.
    let activate = ConnectorActivate {
        host: host.clone(),
        dirs: dirs.clone(),
        inspected: Default::default(),
    };
    let _ = activate.policy(&c, &json!({ "name": "pets" }));
    let done = activate.call(&c, json!({ "name": "pets" })).await;
    assert!(!done.is_error, "{}", done.content);
    assert!(dirs.active_fixtures("pets").exists() && !file.exists());
}

#[tokio::test]
async fn query_keys_never_reach_the_tape() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/items"))
        .and(query_param("token", "q-secret-123"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({ "items": [1, 2], "url": "/v1/items?token=q-secret-123" })),
        )
        .mount(&server)
        .await;
    let spec = json!({
        "swagger": "2.0", "info": { "title": "Items", "version": "1" },
        "host": server.address().to_string(), "basePath": "/v1", "schemes": ["http"],
        "securityDefinitions": { "k": { "type": "apiKey", "in": "query", "name": "token" } },
        "security": [{ "k": [] }],
        "paths": { "/items": { "get": { "operationId": "listItems" } } }
    });
    let mut doc = openapi::parse(&spec.to_string(), None, None).unwrap();
    // A Swagger document without https is reached over http as written.
    doc.base_url = format!("{}/v1", server.uri());
    let g = openapi::generate(&doc, "items", &[0], "").unwrap();
    let (host, _tmp) = host_with(&[("ITEMS_API_KEY", "q-secret-123")]);
    let vm = Vm::load(&host, "items.luau", &g.source).unwrap();
    let (live, case) = fixtures::record_case(&vm, "list_items", &json!({}), None)
        .await
        .unwrap();
    assert!(
        live.contains("[REDACTED]") && !live.contains("q-secret-123"),
        "{live}"
    );
    let text = serde_json::to_string(&case).unwrap();
    assert!(!text.contains("q-secret-123"), "{text}");
    assert!(
        !case.exchanges[0].request.url.contains("token"),
        "injected after recording: {}",
        case.exchanges[0].request.url
    );
}
