//! Tests for `manage_repo`: deleting agents and channels.
#![allow(clippy::unwrap_used)]

use crate::{
    AgentChange, AgentRow, ChannelRow, DEFAULT_AGENT, HOME, NewPost, NewThread, RoutineFate,
    ScheduleRow, Store,
};

fn agent(s: &Store, slug: &str) -> (AgentRow, String) {
    s.create_agent(&AgentRow {
        id: format!("tern:{slug}"),
        slug: slug.into(),
        name: slug.to_uppercase(),
        memory: "private".into(),
        ..AgentRow::default()
    })
    .unwrap()
}

fn topic(s: &Store, slug: &str) -> ChannelRow {
    s.create_channel(&ChannelRow {
        id: format!("c-{slug}"),
        slug: slug.into(),
        title: slug.into(),
        kind: "topic".into(),
        created_by: "user".into(),
        ..ChannelRow::default()
    })
    .unwrap()
}

fn routine(s: &Store, id: &str, agent: &str, channel: Option<&str>) {
    s.create_session(&format!("s-{id}"), "cron", None).unwrap();
    s.create_schedule(&ScheduleRow {
        id: id.into(),
        cron: "0 8 * * *".into(),
        prompt: format!("do {id}\nmore"),
        session_id: format!("s-{id}"),
        enabled: true,
        ..Default::default()
    })
    .unwrap();
    s.set_schedule_agent(id, agent).unwrap();
    if let Some(c) = channel {
        s.set_schedule_channel(id, c).unwrap();
    }
}

#[test]
fn deleting_an_agent_tombstones_it_and_keeps_history_resolvable() {
    let s = Store::open_in_memory().unwrap();
    let (a, dm) = agent(&s, "research");
    let (b, _) = agent(&s, "ops");
    s.update_agent(
        &b.id,
        &AgentChange {
            delegates: Some(Some(r#"["tern:research","tern"]"#.into())),
            ..Default::default()
        },
    )
    .unwrap();
    routine(&s, "r1", &a.id, Some(&dm));
    s.save_agent_memory("research secret", &[], Some(&a.id))
        .unwrap();
    s.create_thread(&NewThread {
        id: "conv",
        source: "api",
        channel_id: &dm,
        ..Default::default()
    })
    .unwrap();
    s.add_post(&NewPost {
        channel_id: &dm,
        author_kind: "agent",
        author_id: &a.id,
        author: "RESEARCH",
        kind: "answer",
        text: "hello",
        ..NewPost::default()
    })
    .unwrap();
    let used = s.agent_use(&a.id).unwrap();
    assert_eq!(used.routines, [("r1".to_owned(), "do r1".to_owned())]);
    assert_eq!((used.conversations, used.memories), (2, 1));

    assert!(
        s.delete_agent(&a.id, &RoutineFate::Move(DEFAULT_AGENT.into()))
            .unwrap()
    );
    // Gone from every lookup and list, slug freed.
    assert!(s.agent(&a.id).unwrap().is_none());
    assert!(s.agent("research").unwrap().is_none());
    assert!(s.agents(true).unwrap().iter().all(|x| x.id != a.id));
    // But its id still resolves for history.
    let t = s.agent_or_deleted(&a.id).unwrap().unwrap();
    assert_eq!(t.name, "RESEARCH");
    assert!(t.deleted_at.is_some());
    assert_eq!(s.deleted_agents().unwrap().len(), 1);
    // Its conversation, posts and DM remain (the DM hidden).
    assert_eq!(s.session_agent("conv").unwrap(), a.id);
    assert_eq!(s.posts(&dm, None, 10).unwrap().len(), 1);
    assert!(s.channel(&dm).unwrap().unwrap().archived_at.is_some());
    assert!(s.channel("dm-research").unwrap().is_none());
    // Its routine is now tern's, delivering to Home; memories and
    // delegations are cleaned up.
    let r = s.schedule("r1").unwrap().unwrap();
    assert_eq!((r.agent(), r.channel_id.as_deref()), ("tern", Some("home")));
    assert_eq!(s.session_agent("s-r1").unwrap(), DEFAULT_AGENT);
    assert_eq!(s.list_memories(None, None, 10).unwrap().len(), 0);
    let b = s.agent(&b.id).unwrap().unwrap();
    assert_eq!(b.delegate_list().unwrap(), ["tern"]);
    // The slug can be taken again.
    agent(&s, "research2");
    s.update_agent(
        "tern:research2",
        &AgentChange {
            slug: Some("research".into()),
            ..Default::default()
        },
    )
    .unwrap();
    // The ledger recorded it.
    assert!(
        s.ledger(&crate::LedgerFilter::default())
            .unwrap()
            .iter()
            .any(|e| e.entry.kind == "agent.delete" && e.entry.subject == a.id)
    );
}

#[test]
fn deleting_an_agent_can_delete_its_routines_and_refuses_the_default_or_a_lead() {
    let s = Store::open_in_memory().unwrap();
    let (a, _) = agent(&s, "research");
    routine(&s, "r1", &a.id, None);
    let c = topic(&s, "launch");
    s.set_channel_lead(&c.id, &a.id).unwrap();
    assert!(
        s.delete_agent(&a.id, &RoutineFate::Delete).is_err(),
        "hand over the channel first"
    );
    s.set_channel_lead(&c.id, DEFAULT_AGENT).unwrap();
    assert!(s.delete_agent(&a.id, &RoutineFate::Delete).unwrap());
    assert!(s.schedule("r1").unwrap().is_none());
    assert!(s.delete_agent(DEFAULT_AGENT, &RoutineFate::Delete).is_err());
    assert!(!s.delete_agent("tern:nobody", &RoutineFate::Delete).unwrap());
}

#[test]
fn deleting_a_channel_removes_its_posts_but_keeps_conversations() {
    let s = Store::open_in_memory().unwrap();
    let (a, dm) = agent(&s, "research");
    let c = topic(&s, "launch");
    routine(&s, "r1", &a.id, Some(&c.id));
    s.add_channel_agent(&c, &a.id).unwrap();
    s.create_thread(&NewThread {
        id: "conv",
        source: "api",
        channel_id: &c.id,
        ..Default::default()
    })
    .unwrap();
    let (p, _) = s
        .add_post(&NewPost {
            channel_id: &c.id,
            author_kind: "agent",
            author_id: "tern",
            author: "tern",
            kind: "needs_you",
            text: "approve?",
            ..NewPost::default()
        })
        .unwrap();
    // Home has a pointer to the needs-you post.
    assert!(s.pointer_to(p.id).unwrap().is_some());
    let used = s.channel_use(&c.id).unwrap();
    assert_eq!((used.conversations, used.members), (1, 1));
    assert_eq!(used.routines.len(), 1);

    assert!(s.set_channel_slug(&dm, "x").is_err(), "DMs follow agents");
    assert!(s.set_channel_slug(&c.id, "launches").unwrap());
    assert!(s.channel("launches").unwrap().is_some());
    assert!(s.delete_channel(&dm).is_err());
    assert!(s.delete_channel(HOME).is_err());

    assert!(s.delete_channel(&c.id).unwrap());
    assert!(s.channel(&c.id).unwrap().is_none());
    assert!(s.posts(&c.id, None, 10).unwrap().is_empty());
    assert!(
        s.posts(HOME, None, 10)
            .unwrap()
            .iter()
            .all(|p| p.kind != "pointer")
    );
    // The conversation is still there, under Home.
    let conv = s.session("conv").unwrap().unwrap();
    assert_eq!(conv.channel_id.as_deref(), Some(HOME));
    // The routine posts to its agent's DM now.
    assert_eq!(
        s.schedule("r1").unwrap().unwrap().channel_id.as_deref(),
        Some(dm.as_str())
    );
    assert!(!s.delete_channel(&c.id).unwrap(), "already gone");
}

#[test]
fn deleting_an_agent_drops_its_bot_reference_only() {
    let s = Store::open_in_memory().unwrap();
    s.create_agent(&AgentRow {
        id: "tern:bot".into(),
        slug: "bot".into(),
        name: "Bot".into(),
        memory: "private".into(),
        telegram: Some(r#"{"bot_token_secret":"my_bot_token"}"#.into()),
        ..AgentRow::default()
    })
    .unwrap();
    assert!(s.delete_agent("tern:bot", &RoutineFate::Delete).unwrap());
    // The tombstone has no bot, so no poller is built for it, and the
    // ledger remembers which secret it named.
    let t = s.agent_or_deleted("tern:bot").unwrap().unwrap();
    assert_eq!(t.telegram, None);
    let rows = s.ledger(&crate::LedgerFilter::default()).unwrap();
    let e = rows
        .iter()
        .find(|e| e.entry.kind == "agent.delete")
        .unwrap();
    assert!(
        e.entry
            .detail
            .as_ref()
            .unwrap()
            .to_string()
            .contains("my_bot_token")
    );
}
