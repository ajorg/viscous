import 'package:flutter/material.dart';

import 'camera_screen.dart';
import 'src/rust/frb_generated.dart';

Future<void> main() async {
  await RustLib.init();
  runApp(const ViscousApp());
}

class ViscousApp extends StatelessWidget {
  const ViscousApp({super.key});

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'Viscous',
      theme: ThemeData(
        colorScheme: ColorScheme.fromSeed(seedColor: Colors.teal),
        useMaterial3: true,
      ),
      darkTheme: ThemeData(
        colorScheme: ColorScheme.fromSeed(
          seedColor: Colors.teal,
          brightness: Brightness.dark,
        ),
        useMaterial3: true,
      ),
      home: const CameraScreen(),
    );
  }
}
