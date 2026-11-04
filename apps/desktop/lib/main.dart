import 'package:flutter/material.dart';
import 'package:tern_client/tern_client.dart';

import 'state/connection.dart';
import 'state/daemon.dart';
import 'state/notifier.dart';
import 'state/store.dart';
import 'theme.dart';
import 'ui/home.dart';

Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  final prefs = AppPrefs.load();
  final tokens = TokenStore.platform();
  final endpoint = await endpointFor(prefs.connection, tokens);
  // A server whose token is gone can't be used: ask again where to run.
  if (endpoint == null) prefs.connection = null;
  runApp(
    TernApp(
      prefs: prefs,
      tokens: tokens,
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
  });

  final AppPrefs prefs;
  final TokenStore tokens;
  final Endpoint endpoint;

  @override
  State<TernApp> createState() => _TernAppState();
}

class _TernAppState extends State<TernApp> {
  late TernStore _store = _storeFor(widget.endpoint);
  late final _connections = Connections(
    prefs: widget.prefs,
    tokens: widget.tokens,
    apply: _switchTo,
  );

  static TernStore _storeFor(Endpoint e) =>
      TernStore(TernClient(e), notifier: Notifier.platform())..connect();

  void _switchTo(Endpoint e) {
    final old = _store;
    setState(() => _store = _storeFor(e));
    // After the frame that stops using it.
    WidgetsBinding.instance.addPostFrameCallback((_) => old.dispose());
  }

  @override
  void dispose() {
    _store.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) => MaterialApp(
    title: 'tern',
    debugShowCheckedModeBanner: false,
    theme: ternTheme(TernColors.light, Brightness.light),
    darkTheme: ternTheme(TernColors.dark, Brightness.dark),
    home: Home(
      key: ObjectKey(_store),
      store: _store,
      prefs: widget.prefs,
      connections: _connections,
    ),
  );
}
