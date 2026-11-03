import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'endpoint.dart';
import 'models.dart';
import 'sse.dart';

/// An error response from the daemon (`{"error": "..."}`) or a failure to
/// reach it.
class TernException implements Exception {
  const TernException(this.message, {this.status});

  final String message;
  final int? status;

  bool get unreachable => status == null;

  @override
  String toString() => message;
}

class TernClient {
  TernClient(this.endpoint) : _http = endpoint.httpClient();

  final Endpoint endpoint;
  final HttpClient _http;

  void close() => _http.close(force: true);

  Future<Health> health() async =>
      Health.fromJson(await _json('GET', '/v1/health'));

  Future<Info> info() async => Info.fromJson(await _json('GET', '/v1/info'));

  /// Throws [TernException] with the daemon's reason if `model` is unusable.
  Future<void> checkModel(String model) =>
      _json('POST', '/v1/models/check', {'model': model});

  Future<List<SessionItem>> sessions({int limit = 50}) async => [
    for (final s in await _json('GET', '/v1/sessions?limit=$limit') as List)
      SessionItem.fromJson(s as Json),
  ];

  Future<SessionDetail> session(String id) async =>
      SessionDetail.fromJson(await _json('GET', '/v1/sessions/$id'));

  Future<void> setModel(String session, String model) =>
      _json('PUT', '/v1/sessions/$session/model', {'model': model});

  Future<Usage> usage(String session) async =>
      Usage.fromJson(await _json('GET', '/v1/sessions/$session/usage'));

  /// Send a message. Without `session` the daemon starts a new one. A run
  /// already in progress in that session is cancelled first.
  Future<Started> send(String text, {String? session, String? model}) async =>
      Started.fromJson(
        await _json('POST', '/v1/runs', {
          'text': text,
          'session_id': ?session,
          'model': ?model,
        }),
      );

  Future<bool> cancel(String run) async =>
      (await _json('POST', '/v1/runs/$run/cancel'))['cancelled'] as bool;

  Future<void> resume(String run) => _json('POST', '/v1/runs/$run/resume');

  Future<List<ActiveRun>> activeRuns() async => [
    for (final r in await _json('GET', '/v1/runs/active') as List)
      ActiveRun.fromJson(r as Json),
  ];

  Future<List<Approval>> pendingApprovals() async => [
    for (final a in await _json('GET', '/v1/approvals') as List)
      Approval.fromJson(a as Json),
  ];

  Future<List<Schedule>> schedules() async => [
    for (final s in await _json('GET', '/v1/schedules') as List)
      Schedule.fromJson(s as Json),
  ];

  /// `deliverTo`: null keeps results in the routine's session; else
  /// `telegram:<chat_id>`.
  Future<Schedule> createSchedule({
    required String cron,
    required String prompt,
    String? deliverTo,
  }) async => Schedule.fromJson(
    await _json('POST', '/v1/schedules', {
      'cron': cron,
      'prompt': prompt,
      'deliver_to': ?deliverTo,
    }),
  );

  /// Pass `deliverTo: 'none'` to stop delivering.
  Future<Schedule> updateSchedule(
    String id, {
    String? cron,
    String? prompt,
    bool? enabled,
    String? deliverTo,
  }) async => Schedule.fromJson(
    await _json('PATCH', '/v1/schedules/$id', {
      'cron': ?cron,
      'prompt': ?prompt,
      'enabled': ?enabled,
      'deliver_to': ?deliverTo,
    }),
  );

  Future<void> deleteSchedule(String id) =>
      _json('DELETE', '/v1/schedules/$id');

  /// Run a routine now; its next scheduled run doesn't move.
  Future<Started> runSchedule(String id) async {
    final j = await _json('POST', '/v1/schedules/$id/run') as Json;
    return Started.fromJson({...j, 'model': ''});
  }

  /// Validate `cron` and get its next three runs. Throws [TernException]
  /// with the reason if it's invalid.
  Future<CronPreview> previewCron(String cron) async => CronPreview.fromJson(
    await _json('POST', '/v1/schedules/preview', {'cron': cron}),
  );

  Future<List<RunItem>> runs(String session) async => [
    for (final r in await _json('GET', '/v1/sessions/$session/runs') as List)
      RunItem.fromJson(r as Json),
  ];

  Future<List<StepItem>> steps(String run) async => [
    for (final s in await _json('GET', '/v1/runs/$run/steps') as List)
      StepItem.fromJson(s as Json),
  ];

  /// Every secret in `secrets.toml` or referred to by config or a
  /// connector. Values are never returned.
  Future<List<SecretStatus>> secrets() async => [
    for (final s in await _json('GET', '/v1/secrets') as List)
      SecretStatus.fromJson(s as Json),
  ];

  /// Store `value` in `secrets.toml`. Takes effect without a restart.
  Future<SecretStatus> setSecret(String name, String value) async =>
      SecretStatus.fromJson(
        await _json('PUT', '/v1/secrets/$name', {'value': value}),
      );

  /// Remove `name` from `secrets.toml`. Returns false if it wasn't there.
  Future<bool> removeSecret(String name) async {
    try {
      await _json('DELETE', '/v1/secrets/$name');
      return true;
    } on TernException catch (e) {
      if (e.status == 404) return false;
      rethrow;
    }
  }

  Future<void> answer(String approval, Decision decision) =>
      _json('POST', '/v1/approvals/$approval', {'decision': decision.wire});

  /// The live event stream. It ends (with an error) when the connection
  /// drops; callers reconnect and resync. Cancelling closes the connection.
  Stream<TernEvent> events() {
    // Its own HttpClient: cancelling an open response doesn't return until
    // the server ends it, so the connection is force-closed instead.
    final http = endpoint.httpClient();
    final out = StreamController<TernEvent>();
    out.onListen = () async {
      final HttpClientResponse res;
      try {
        res = await _send(
          'GET',
          '/v1/events',
          null,
          client: http,
          accept: 'text/event-stream',
        );
      } on TernException catch (e) {
        out.addError(e);
        await out.close();
        return;
      }
      res
          .transform(const SseDecoder())
          .listen(
            (data) {
              final Object? decoded;
              try {
                decoded = jsonDecode(data);
              } on FormatException {
                return;
              }
              if (decoded is Json) out.add(TernEvent.fromJson(decoded));
            },
            onError: (Object e) =>
                out.addError(TernException('event stream lost: $e')),
            onDone: out.close,
            cancelOnError: true,
          );
    };
    out.onCancel = () => http.close(force: true);
    return out.stream;
  }

  Future<dynamic> _json(String method, String path, [Object? body]) async {
    final res = await _send(method, path, body);
    final text = await res.transform(utf8.decoder).join();
    final Object? decoded = text.isEmpty ? null : jsonDecode(text);
    if (res.statusCode >= 400) {
      final msg = decoded is Json ? decoded['error'] : null;
      throw TernException(
        msg as String? ?? 'HTTP ${res.statusCode}',
        status: res.statusCode,
      );
    }
    return decoded;
  }

  Future<HttpClientResponse> _send(
    String method,
    String path,
    Object? body, {
    HttpClient? client,
    String accept = 'application/json',
  }) async {
    try {
      final req = await (client ?? _http).openUrl(
        method,
        endpoint.base.resolve(path),
      );
      req.headers.set(HttpHeaders.acceptHeader, accept);
      endpoint.headers.forEach(req.headers.set);
      if (body != null) {
        req.headers.contentType = ContentType.json;
        req.write(jsonEncode(body));
      }
      return await req.close();
    } on SocketException catch (e) {
      throw TernException(
        "can't reach tern at ${endpoint.describe()}: "
        '${e.osError?.message ?? e.message}',
      );
    } on HttpException catch (e) {
      throw TernException(e.message);
    }
  }
}
