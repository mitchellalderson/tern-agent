import 'package:flutter/material.dart';
import 'package:tern_client/tern_client.dart';

import 'state/account.dart';
import 'state/connection.dart';
import 'state/daemon.dart';
import 'state/notifier.dart';
import 'state/store.dart';
import 'theme.dart';
import 'ui/home.dart';

Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  final prefs = AppPrefs.load();
  final stored = TokenStore.platform();
  // Accounts exist only in a build or environment with a Clerk key.
  final clerk = ClerkConfig.fromEnvironment();
  final account = clerk == null
      ? null
      : AccountService(config: clerk, tokens: stored);
  await account?.load();
  final tokens = account == null ? stored : AccountTokens(stored, account);
  final endpoint = await endpointFor(prefs.connection, tokens);
  // A server whose token is gone can't be used: ask again where to run.
  if (endpoint == null) prefs.connection = null;
  runApp(
    TernApp(
      prefs: prefs,
      tokens: tokens,
      account: account,
      endpoint: endpoint ?? Endpoint.local(),
    ),
  );
}

/// Holds the store for the chosen endpoint, and makes a new one when the
/// user switches between this computer and a server.
class TernApp extends StatefulWidget {
  const TernApp({
    super.key,
    required this.prefs,
    required this.tokens,
    required this.endpoint,
    this.account,
  });

  final AppPrefs prefs;
  final TokenStore tokens;
  final AccountService? account;
  final Endpoint endpoint;

  @override
  State<TernApp> createState() => _TernAppState();
}

class _TernAppState extends State<TernApp> {
  /// The store for the server in use; null for the frame between one
  /// server's window and the next.
  late TernStore? _store = _storeFor(widget.endpoint, widget.prefs);

  /// Where the window goes next while switching.
  Endpoint? _next;
  late final _connections = Connections(
    prefs: widget.prefs,
    tokens: widget.tokens,
    apply: _switchTo,
    account: widget.account,
  );

  static TernStore _storeFor(Endpoint e, AppPrefs prefs) => TernStore(
    TernClient(e),
    notifier: Notifier.platform(),
    lastSeen: prefs.read,
    onReadChanged: prefs.save,
  )..connect();

  /// Another server (or this computer): the window for the old one is torn
  /// down completely first (its streams, caches, selection, drafts), then
  /// a new store connects to [e] alone. A second switch before that
  /// finishes just changes where it ends up.
  void _switchTo(Endpoint e) {
    _next = e;
    final old = _store;
    if (old == null) return;
    setState(() => _store = null);
    // After the frame that stops using it.
    WidgetsBinding.instance.addPostFrameCallback((_) {
      old.dispose();
      if (!mounted) return;
      setState(() => _store = _storeFor(_next!, widget.prefs));
    });
  }

  @override
  void dispose() {
    _store?.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) => MaterialApp(
    title: 'tern',
    debugShowCheckedModeBanner: false,
    theme: ternTheme(TernColors.light, Brightness.light),
    darkTheme: ternTheme(TernColors.dark, Brightness.dark),
    home: switch (_store) {
      final store? => Home(
        key: ObjectKey(store),
        store: store,
        prefs: widget.prefs,
        connections: _connections,
      ),
      null => const Scaffold(body: Center(child: CircularProgressIndicator())),
    },
  );
}
