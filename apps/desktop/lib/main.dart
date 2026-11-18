import 'package:flutter/material.dart';
import 'package:tern_client/tern_client.dart';

import 'state/daemon.dart';
import 'state/notifier.dart';
import 'state/store.dart';
import 'theme.dart';
import 'ui/home.dart';

Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  runApp(TernApp(prefs: AppPrefs.load(), endpoint: Endpoint.local()));
}

/// Holds the store for tern's local daemon (over its Unix socket).
class TernApp extends StatefulWidget {
  const TernApp({super.key, required this.prefs, required this.endpoint});

  final AppPrefs prefs;
  final Endpoint endpoint;

  @override
  State<TernApp> createState() => _TernAppState();
}

class _TernAppState extends State<TernApp> {
  late final TernStore _store = TernStore(
    TernClient(widget.endpoint),
    notifier: Notifier.platform(),
    lastSeen: widget.prefs.read,
    onReadChanged: widget.prefs.save,
  )..connect();

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
    home: Home(store: _store, prefs: widget.prefs),
  );
}
