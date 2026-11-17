import socket
import vgamepad as vg
import ast
import time

UDP_IP = "0.0.0.0"
UDP_PORT = 9876
DEADZONE = 0.1

gamepad = vg.VDS4Gamepad()  # DS4 instead of Xbox
sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
sock.bind((UDP_IP, UDP_PORT))

print(f"🎮 Listening on UDP {UDP_PORT} for DS4 input...")

# DS4 button mapping
BUTTON_MAP = {
    0: vg.DS4_BUTTONS.DS4_BUTTON_CROSS,      # X (Bottom)
    1: vg.DS4_BUTTONS.DS4_BUTTON_CIRCLE,     # O (Right)
    2: vg.DS4_BUTTONS.DS4_BUTTON_TRIANGLE,   # △ (Top)
    3: vg.DS4_BUTTONS.DS4_BUTTON_SQUARE,     # ◻ (Left)
    4: vg.DS4_BUTTONS.DS4_BUTTON_SHOULDER_LEFT,
    5: vg.DS4_BUTTONS.DS4_BUTTON_SHOULDER_RIGHT,
    6: vg.DS4_BUTTONS.DS4_BUTTON_THUMB_LEFT,
    7: vg.DS4_BUTTONS.DS4_BUTTON_THUMB_RIGHT,
    8: vg.DS4_BUTTONS.DS4_BUTTON_SHARE,
    9: vg.DS4_BUTTONS.DS4_BUTTON_OPTIONS,
}

DPAD_MAP = {
    (-1, 0): vg.DS4_DPAD_DIRECTIONS.DS4_BUTTON_DPAD_WEST,
    (-1, 1): vg.DS4_DPAD_DIRECTIONS.DS4_BUTTON_DPAD_NORTHWEST,
    (0, 1): vg.DS4_DPAD_DIRECTIONS.DS4_BUTTON_DPAD_NORTH,
    (1, 1): vg.DS4_DPAD_DIRECTIONS.DS4_BUTTON_DPAD_NORTHEAST,
    (1, 0): vg.DS4_DPAD_DIRECTIONS.DS4_BUTTON_DPAD_EAST,
    (1, -1): vg.DS4_DPAD_DIRECTIONS.DS4_BUTTON_DPAD_SOUTHEAST,
    (0, -1): vg.DS4_DPAD_DIRECTIONS.DS4_BUTTON_DPAD_SOUTH,
    (-1, -1): vg.DS4_DPAD_DIRECTIONS.DS4_BUTTON_DPAD_SOUTHWEST,
    (0, 0): vg.DS4_DPAD_DIRECTIONS.DS4_BUTTON_DPAD_NONE
}

def normalize_axis(value, invert=False):
    if abs(value) < DEADZONE:
        value = 0.0
    value = max(-1.0, min(1.0, value))
    scaled = int(((-value if invert else value) + 1.0) * 127.5)
    return min(255, max(0, scaled))

try:
    while True:
        data, addr = sock.recvfrom(1024)
        msg = data.decode('utf-8')

        parts = msg.split("|")
        if len(parts) != 4:
            continue

        _, buttons_str, axes_str, hats_str = parts

        try:
            buttons = ast.literal_eval(buttons_str)
            axes = ast.literal_eval(axes_str)
            hats = ast.literal_eval(hats_str)
        except (SyntaxError, ValueError):
            continue

        # Buttons
        for idx, state in enumerate(buttons):
            if idx in BUTTON_MAP:
                if state:
                    gamepad.press_button(BUTTON_MAP[idx])
                else:
                    gamepad.release_button(BUTTON_MAP[idx])

        # Left stick
        gamepad.left_joystick(
            x_value=normalize_axis(axes[0]),
            y_value=normalize_axis(axes[1])
        )

        # Right stick
        gamepad.right_joystick(
            x_value=normalize_axis(axes[3]),
            y_value=normalize_axis(axes[4])
        )

        # Triggers (DS4 expects 0-255)
        gamepad.left_trigger(normalize_axis(axes[2]))   # L2
        gamepad.right_trigger(normalize_axis(axes[5]))  # R2

        # D-pad
        if hats:
            dpad_dir = DPAD_MAP.get(hats[0], vg.DS4_DPAD_DIRECTIONS.DS4_BUTTON_DPAD_NONE)
            gamepad.directional_pad(dpad_dir)


        gamepad.update()
        time.sleep(0.01)

except KeyboardInterrupt:
    print("\n👋 Exiting gracefully.")
finally:
    sock.close()
