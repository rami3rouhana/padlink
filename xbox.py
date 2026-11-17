import socket
import vgamepad as vg
import ast
import time

UDP_IP = "0.0.0.0"  # Listen on all interfaces
UDP_PORT = 9876
DEADZONE = 0.1  # Adjust deadzone threshold as needed (0.05-0.2)

gamepad = vg.VX360Gamepad()
sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
sock.bind((UDP_IP, UDP_PORT))

print(f"Listening on port {UDP_PORT}...")

# Button mapping for Xbox 360
BUTTON_MAP = {
    0: vg.XUSB_BUTTON.XUSB_GAMEPAD_A,
    1: vg.XUSB_BUTTON.XUSB_GAMEPAD_B,
    2: vg.XUSB_BUTTON.XUSB_GAMEPAD_Y,
    3: vg.XUSB_BUTTON.XUSB_GAMEPAD_X,
    4: vg.XUSB_BUTTON.XUSB_GAMEPAD_LEFT_SHOULDER,
    5: vg.XUSB_BUTTON.XUSB_GAMEPAD_RIGHT_SHOULDER,
    8: vg.XUSB_BUTTON.XUSB_GAMEPAD_BACK,
    9: vg.XUSB_BUTTON.XUSB_GAMEPAD_START,
    10: vg.XUSB_BUTTON.XUSB_GAMEPAD_GUIDE,
}

def normalize_axis(value, invert=False):
    """Normalize axis with deadzone and ensure full range (-1.0 to 1.0)"""
    if abs(value) < DEADZONE:
        value = 0.0
    value = max(-1.0, min(1.0, value))  # Clamp to valid range
    return int((-value if invert else value) * 32767)

def print_axis_debug(axes):
    """Debug output for stick positions"""
    print(f"\nLeft Stick:  X={axes[0]:+1.3f}, Y={axes[1]:+1.3f}")
    print(f"Right Stick: X={axes[2]:+1.3f}, Y={axes[3]:+1.3f}")
    print(f"Triggers:    L2={axes[4]:+1.3f}, R2={axes[5]:+1.3f}")

try:
    while True:
        data, addr = sock.recvfrom(1024)
        msg = data.decode('utf-8')
        
        parts = msg.split("|")
        if len(parts) != 4:
            continue
        
        name, buttons_str, axes_str, hats_str = parts
        
        try:
            buttons = ast.literal_eval(buttons_str)
            axes = ast.literal_eval(axes_str)
            hats = ast.literal_eval(hats_str)
        except (SyntaxError, ValueError):
            continue

        # Debug output (uncomment to verify corner reach)
        # print_axis_debug(axes)

        # Update buttons
        for btn_idx, btn_state in enumerate(buttons):
            if btn_idx in BUTTON_MAP:
                if btn_state:
                    gamepad.press_button(BUTTON_MAP[btn_idx])
                else:
                    gamepad.release_button(BUTTON_MAP[btn_idx])

        # Update sticks (no inversion by default)
        gamepad.left_joystick(
            x_value=normalize_axis(axes[0]),
            y_value=normalize_axis(axes[1], invert=True)  # Y typically needs inversion
        )
        gamepad.right_joystick(
            x_value=normalize_axis(axes[3]),
            y_value=normalize_axis(axes[4], invert=True)  # Invert Y for consistency
        )

        # Triggers (0.0 to 1.0 range)
        gamepad.left_trigger(int((axes[2] + 1) * 127.5))
        gamepad.right_trigger(int((axes[5] + 1) * 127.5))

        # D-pad (hat switch)
        if hats and hats[0]:
            dpad_x, dpad_y = hats[0]
            gamepad.release_button(vg.XUSB_BUTTON.XUSB_GAMEPAD_DPAD_UP)
            gamepad.release_button(vg.XUSB_BUTTON.XUSB_GAMEPAD_DPAD_DOWN)
            gamepad.release_button(vg.XUSB_BUTTON.XUSB_GAMEPAD_DPAD_LEFT)
            gamepad.release_button(vg.XUSB_BUTTON.XUSB_GAMEPAD_DPAD_RIGHT)
            
            if dpad_y == 1:
                gamepad.press_button(vg.XUSB_BUTTON.XUSB_GAMEPAD_DPAD_UP)
            elif dpad_y == -1:
                gamepad.press_button(vg.XUSB_BUTTON.XUSB_GAMEPAD_DPAD_DOWN)
            if dpad_x == -1:
                gamepad.press_button(vg.XUSB_BUTTON.XUSB_GAMEPAD_DPAD_LEFT)
            elif dpad_x == 1:
                gamepad.press_button(vg.XUSB_BUTTON.XUSB_GAMEPAD_DPAD_RIGHT)

        gamepad.update()
        time.sleep(0.01)  # Reduce CPU usage

except KeyboardInterrupt:
    print("\nExiting...")
finally:
    sock.close()