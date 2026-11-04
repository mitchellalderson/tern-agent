import 'models.dart';

/// A channel (docs/channels-design.md): Home, the user's direct line to
/// the agent, or a topic timeline such as `#finances`.
class Channel {
  const Channel({
    required this.id,
    required this.slug,
    required this.title,
    required this.kind,
    this.instructions,
    this.mainSessionId,
    this.lastPostId,
    this.needsYou = 0,
    this.activeRuns = 0,
    this.archived = false,
    this.agentId,
    required this.updatedAt,
  });

  factory Channel.fromJson(Json j) => Channel(
    id: j['id'] as String,
    slug: j['slug'] as String,
    title: j['title'] as String,
    kind: j['kind'] as String,
    instructions: j['instructions'] as String?,
    mainSessionId: j['main_session_id'] as String?,
    lastPostId: j['last_post_id'] as int?,
    needsYou: j['needs_you'] as int? ?? 0,
    activeRuns: j['active_runs'] as int? ?? 0,
    archived: j['archived'] as bool? ?? false,
    agentId: j['agent_id'] as String?,
    updatedAt: _time(j['updated_at'] as int? ?? 0),
  );

  final String id;
  final String slug;
  final String title;

  /// `main` (Home) or `topic`.
  final String kind;
  final String? instructions;

  /// The main thread; for Home, the current segment of its stream.
  final String? mainSessionId;
  final int? lastPostId;

  /// Home: approvals and secret requests waiting for an answer.
  final int needsYou;

  /// Runs in progress in its threads.
  final int activeRuns;
  final bool archived;

  /// A main channel: whose DM it is (`tern` for Home); a topic channel:
  /// its lead.
  final String? agentId;
  final DateTime updatedAt;

  /// The default agent's DM. Older cores have no other main channel.
  bool get isHome => id == 'home' || (kind == 'main' && agentId == null);

  /// Home or an agent's DM: a stream, not a timeline.
  bool get isMain => kind == 'main';

  /// An agent's DM other than Home.
  bool get isDm => isMain && !isHome;
}

/// What an agent, a routine or the user said in a channel.
class Post {
  const Post({
    required this.id,
    required this.channelId,
    this.sessionId,
    this.runId,
    required this.authorKind,
    required this.authorId,
    required this.author,
    required this.kind,
    required this.text,
    this.replyTo,
    this.pointsTo,
    this.tainted = false,
    this.replies = 0,
    this.resolvedAt,
    this.pointsToChannel,
    this.outboxId,
    required this.createdAt,
  });

  factory Post.fromJson(Json j) => Post(
    id: j['id'] as int,
    channelId: j['channel_id'] as String,
    sessionId: j['session_id'] as String?,
    runId: j['run_id'] as String?,
    authorKind: j['author_kind'] as String,
    authorId: j['author_id'] as String,
    author: j['author'] as String,
    kind: j['kind'] as String,
    text: j['text'] as String,
    replyTo: j['reply_to'] as int?,
    pointsTo: j['points_to'] as int?,
    tainted: j['tainted'] as bool? ?? false,
    replies: j['replies'] as int? ?? 0,
    resolvedAt: switch (j['resolved_at']) {
      final int t => _time(t),
      _ => null,
    },
    pointsToChannel: j['points_to_channel'] as String?,
    outboxId: j['outbox_id'] as int?,
    createdAt: _time(j['created_at'] as int),
  );

  final int id;
  final String channelId;

  /// The thread it came from (a routine's own session for its results).
  final String? sessionId;
  final String? runId;

  /// `user`, `agent`, `subagent` or `routine`.
  final String authorKind;

  /// Stable: `owner`, `tern`, `tern/<label>`, `routine:<schedule id>`.
  final String authorId;

  /// What to show: `you`, `tern`, a routine's title.
  final String author;

  /// `message`, `answer`, `update`, `needs_you` or `pointer`.
  final String kind;
  final String text;
  final int? replyTo;
  final int? pointsTo;
  final bool tainted;

  /// Posts replying to this one in a side thread.
  final int replies;

  /// A `needs_you` post or pointer the user marked done.
  final DateTime? resolvedAt;

  /// A pointer: the channel of the post it points to.
  final String? pointsToChannel;

  /// The outbox item this `needs_you` post (or pointer) stands for. It
  /// closes when the item is decided; "Done" alone is refused.
  final int? outboxId;
  final DateTime createdAt;

  bool get mine => authorKind == 'user';

  /// Waiting on the user: a `needs_you` post, or its pointer in Home.
  bool get needsYou =>
      (kind == 'needs_you' || kind == 'pointer') && resolvedAt == null;

  Post resolved() => Post(
    id: id,
    channelId: channelId,
    sessionId: sessionId,
    runId: runId,
    authorKind: authorKind,
    authorId: authorId,
    author: author,
    kind: kind,
    text: text,
    replyTo: replyTo,
    pointsTo: pointsTo,
    tainted: tainted,
    replies: replies,
    resolvedAt: DateTime.now(),
    pointsToChannel: pointsToChannel,
    outboxId: outboxId,
    createdAt: createdAt,
  );
}

/// One entry of a main channel's stream.
sealed class StreamItem {
  const StreamItem();

  factory StreamItem.fromJson(Json j) => switch (j['type']) {
    'message' => StreamMessage(
      sessionId: j['session_id'] as String,
      messageId: j['message_id'] as int,
      message: SessionMessage.fromJson(j),
      queued: j['queued'] as bool? ?? false,
      createdAt: _time(j['created_at'] as int),
    ),
    'post' => StreamPost(Post.fromJson(j['post'] as Json)),
    'cut' => StreamCut(
      sessionId: j['session_id'] as String,
      prevSessionId: j['prev_session_id'] as String?,
      reason: j['reason'] as String?,
      carry: j['carry'] as String?,
      createdAt: _time(j['created_at'] as int),
    ),
    'side_chat' => StreamSideChat(
      sessionId: j['session_id'] as String,
      title: j['title'] as String?,
      branchedFrom: j['branched_from'] as String? ?? '',
      createdAt: _time(j['created_at'] as int),
    ),
    _ => const StreamUnknown(),
  };
}

/// A message of one of the stream's segments.
class StreamMessage extends StreamItem {
  const StreamMessage({
    required this.sessionId,
    required this.messageId,
    required this.message,
    this.queued = false,
    required this.createdAt,
  });
  final String sessionId;
  final int messageId;
  final SessionMessage message;

  /// Sent while a run was working; it read it at its next step.
  final bool queued;
  final DateTime createdAt;
}

/// Something posted from elsewhere: a routine's result, a card.
class StreamPost extends StreamItem {
  const StreamPost(this.post);
  final Post post;
}

/// Where a new segment of the stream starts.
class StreamCut extends StreamItem {
  const StreamCut({
    required this.sessionId,
    this.prevSessionId,
    this.reason,
    this.carry,
    required this.createdAt,
  });
  final String sessionId;
  final String? prevSessionId;

  /// `idle`, `day`, `new_topic` or `migrated`.
  final String? reason;

  /// What the new segment's first run was given about the last one.
  final String? carry;
  final DateTime createdAt;
}

/// A side chat branched off the stream.
class StreamSideChat extends StreamItem {
  const StreamSideChat({
    required this.sessionId,
    this.title,
    required this.branchedFrom,
    required this.createdAt,
  });
  final String sessionId;
  final String? title;
  final String branchedFrom;
  final DateTime createdAt;
}

/// From a newer daemon.
class StreamUnknown extends StreamItem {
  const StreamUnknown();
}

class StreamPage {
  const StreamPage({required this.items, this.next});

  factory StreamPage.fromJson(Json j) => StreamPage(
    items: [for (final i in j['items'] as List) StreamItem.fromJson(i as Json)],
    next: j['next'] as String?,
  );

  /// Newest first.
  final List<StreamItem> items;

  /// The cursor for older items; null at the start of the stream.
  final String? next;
}

/// What a send into a channel did.
class Sent {
  const Sent({
    this.post,
    required this.runId,
    required this.sessionId,
    this.queued = false,
    this.newSegment = false,
  });

  factory Sent.fromJson(Json j) => Sent(
    post: j['post'] is Json ? Post.fromJson(j['post'] as Json) : null,
    runId: j['run_id'] as String,
    sessionId: j['session_id'] as String,
    queued: j['queued'] as bool? ?? false,
    newSegment: j['new_segment'] as bool? ?? false,
  );

  /// The user's post (topic channels; Home's stream is its messages).
  final Post? post;
  final String runId;
  final String sessionId;

  /// It went to the run already working there, which reads it next.
  final bool queued;

  /// Home: a new segment of the stream started.
  final bool newSegment;
}

DateTime _time(int t) =>
    DateTime.fromMillisecondsSinceEpoch(t > 100000000000 ? t : t * 1000);

/// What `POST /v1/channels/move` did.
class MoveResult {
  const MoveResult({required this.channel, required this.moved});

  factory MoveResult.fromJson(Json j) => MoveResult(
    channel: Channel.fromJson(j['channel'] as Json),
    moved: [
      for (final m in j['moved'] as List)
        (
          sessionId: (m as Json)['session_id'] as String,
          from: m['from'] as String,
          fresh: m['fresh_session_id'] as String?,
        ),
    ],
  );

  /// Where they went.
  final Channel channel;

  /// Each conversation, the channel it left, and the empty segment that
  /// took its place in a stream (Home's current one).
  final List<({String sessionId, String from, String? fresh})> moved;
}
