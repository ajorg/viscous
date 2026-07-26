import 'dart:async';

import 'package:flutter/material.dart';

import 'joystick.dart';
import 'src/rust/api/camera.dart';

/// How long a single zoom or focus tap drives the camera for — mirrors the
/// TUI/CLI's fixed-duration nudge per keypress rather than inventing a
/// hold-to-drive interaction the rest of the app doesn't have.
const _tapNudgeMillis = 150;

class CameraScreen extends StatefulWidget {
  const CameraScreen({super.key});

  @override
  State<CameraScreen> createState() => _CameraScreenState();
}

class _CameraScreenState extends State<CameraScreen> {
  final _portController = TextEditingController(text: '/dev/ttyUSB0');
  final _formKey = GlobalKey<FormState>();

  ConnectedInfo? _connection;
  String? _connectError;
  bool _connecting = false;
  CameraState? _cameraState;
  String? _lastCommand;
  StreamSubscription<StatusEvent>? _statusSubscription;

  @override
  void dispose() {
    _statusSubscription?.cancel();
    _portController.dispose();
    super.dispose();
  }

  Future<void> _connect() async {
    setState(() {
      _connecting = true;
      _connectError = null;
    });
    try {
      final info = await connect(port: _portController.text.trim());
      _statusSubscription?.cancel();
      _statusSubscription = subscribeStatus().listen(_applyStatusEvent);
      setState(() => _connection = info);
    } catch (error) {
      setState(() => _connectError = error.toString());
    } finally {
      setState(() => _connecting = false);
    }
  }

  void _applyStatusEvent(StatusEvent event) {
    setState(() {
      switch (event) {
        case StatusEvent_State(:final field0):
          _cameraState = field0;
        case StatusEvent_Command(:final field0):
          _lastCommand = field0;
      }
    });
  }

  @override
  Widget build(BuildContext context) {
    final connection = _connection;
    return Scaffold(
      appBar: AppBar(title: const Text('Viscous')),
      body: SafeArea(
        child: connection == null ? _buildConnectForm() : _buildControls(connection),
      ),
    );
  }

  Widget _buildConnectForm() {
    return Center(
      child: ConstrainedBox(
        constraints: const BoxConstraints(maxWidth: 360),
        child: Padding(
          padding: const EdgeInsets.all(24),
          child: Form(
            key: _formKey,
            child: Column(
              mainAxisSize: MainAxisSize.min,
              children: [
                TextFormField(
                  controller: _portController,
                  decoration: const InputDecoration(
                    labelText: 'Serial port',
                    border: OutlineInputBorder(),
                  ),
                  validator: (value) =>
                      (value == null || value.trim().isEmpty) ? 'Required' : null,
                ),
                const SizedBox(height: 16),
                FilledButton(
                  onPressed: _connecting
                      ? null
                      : () {
                          if (_formKey.currentState!.validate()) _connect();
                        },
                  child: _connecting
                      ? const SizedBox(
                          width: 16,
                          height: 16,
                          child: CircularProgressIndicator(strokeWidth: 2),
                        )
                      : const Text('Connect'),
                ),
                if (_connectError != null) ...[
                  const SizedBox(height: 16),
                  Text(
                    _connectError!,
                    style: TextStyle(color: Theme.of(context).colorScheme.error),
                  ),
                ],
              ],
            ),
          ),
        ),
      ),
    );
  }

  Widget _buildControls(ConnectedInfo connection) {
    return Padding(
      padding: const EdgeInsets.all(24),
      child: Column(
        crossAxisAlignment: CrossAxisAlignment.start,
        children: [
          Text(
            'Connected at ${connection.baudRate} baud — ${connection.summary}',
            style: Theme.of(context).textTheme.bodyMedium,
          ),
          const SizedBox(height: 4),
          Text(
            _lastCommand ?? 'Ready',
            style: Theme.of(context).textTheme.bodySmall,
          ),
          const SizedBox(height: 16),
          Expanded(
            child: Row(
              crossAxisAlignment: CrossAxisAlignment.start,
              children: [
                Expanded(
                  child: Column(
                    mainAxisAlignment: MainAxisAlignment.center,
                    children: [
                      const PanTiltPad(size: 260),
                      const SizedBox(height: 24),
                      _buildZoomFocusRow(),
                    ],
                  ),
                ),
                SizedBox(width: 220, child: _buildPresetList()),
              ],
            ),
          ),
          if (_cameraState case final state?) _buildStateReadout(state),
        ],
      ),
    );
  }

  Widget _buildZoomFocusRow() {
    return Row(
      mainAxisAlignment: MainAxisAlignment.center,
      children: [
        IconButton.filledTonal(
          icon: const Icon(Icons.zoom_out),
          tooltip: 'Zoom out',
          onPressed: () => nudgeZoomOut(millis: BigInt.from(_tapNudgeMillis)),
        ),
        IconButton.filledTonal(
          icon: const Icon(Icons.zoom_in),
          tooltip: 'Zoom in',
          onPressed: () => nudgeZoomIn(millis: BigInt.from(_tapNudgeMillis)),
        ),
        const SizedBox(width: 24),
        IconButton.filledTonal(
          icon: const Icon(Icons.center_focus_strong),
          tooltip: 'Focus near',
          onPressed: () => nudgeFocusNear(millis: BigInt.from(_tapNudgeMillis)),
        ),
        IconButton.filledTonal(
          icon: const Icon(Icons.center_focus_weak),
          tooltip: 'Focus far',
          onPressed: () => nudgeFocusFar(millis: BigInt.from(_tapNudgeMillis)),
        ),
      ],
    );
  }

  Widget _buildPresetList() {
    return ListView.separated(
      itemCount: 6,
      separatorBuilder: (context, index) => const SizedBox(height: 8),
      itemBuilder: (context, index) {
        final number = index + 1;
        return Row(
          children: [
            Expanded(
              child: OutlinedButton(
                onPressed: () => recallPreset(number: number),
                child: Text('Preset $number'),
              ),
            ),
            IconButton(
              icon: const Icon(Icons.bookmark_add_outlined),
              tooltip: 'Save current position to preset $number',
              onPressed: () => savePreset(number: number),
            ),
          ],
        );
      },
    );
  }

  Widget _buildStateReadout(CameraState state) {
    final zoomHex = state.zoom.toRadixString(16).padLeft(4, '0');
    final focusHex = state.focus.toRadixString(16).padLeft(4, '0');
    return Padding(
      padding: const EdgeInsets.only(top: 16),
      child: Text(
        'power=${state.powerOn ? 'on' : 'off'} pan=${state.pan} tilt=${state.tilt} '
        'zoom=0x$zoomHex focus=0x$focusHex',
        style: Theme.of(context).textTheme.bodySmall,
      ),
    );
  }
}
