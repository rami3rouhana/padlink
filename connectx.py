import socket
import ast
import time
import multiprocessing
import vgamepad as vg

UDP_IP = "0.0.0.0"
UDP_PORT = 9876
DEADZONE = 0.1
USE_DS4 = False   # Set to False for Xbox 360

def normalize_axis(val, invert=False, ds4=True):
    if abs(val) < DEADZONE:
        val = 0.0
    val = max(-1.0, min(1.0, val))
    if ds4:
        scaled = int(((-val if invert else val) + 1.0) * 127.5)
        return min(255, max(0, scaled))
    else:
        return int((-val if invert else val) * 32767)

def controller_process(name, queue, use_ds4):
    print(f"[{name}] 🎮 Controller process started")
    gamepad = vg.VDS4Gamepad() if use_ds4 else vg.VX360Gamepad()

    if use_ds4:
        BUTTON_MAP = {
            0: vg.DS4_BUTTONS.DS4_BUTTON_CROSS,
            1: vg.DS4_BUTTONS.DS4_BUTTON_CIRCLE,
            2: vg.DS4_BUTTONS.DS4_BUTTON_TRIANGLE,
            3: vg.DS4_BUTTONS.DS4_BUTTON_SQUARE,
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

    else:
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

    while True:
        try:
            buttons, axes, hats = queue.get(timeout=2)
        except:
            continue

        for idx, state in enumerate(buttons):
            if idx in BUTTON_MAP:
                if state:
                    gamepad.press_button(BUTTON_MAP[idx])
                else:
                    gamepad.release_button(BUTTON_MAP[idx])

        if use_ds4:
            gamepad.left_joystick(x_value=normalize_axis(axes[0], ds4=True),
                                  y_value=normalize_axis(axes[1], ds4=True))
            gamepad.right_joystick(x_value=normalize_axis(axes[3], ds4=True),
                                   y_value=normalize_axis(axes[4], ds4=True))
            gamepad.left_trigger(normalize_axis(axes[2], ds4=True))
            gamepad.right_trigger(normalize_axis(axes[5], ds4=True))
            if hats:
                dpad = vg.DS4_DPAD_DIRECTIONS.DS4_BUTTON_DPAD_NONE
                if hats[0] in DPAD_MAP:
                    dpad = DPAD_MAP[hats[0]]
                gamepad.directional_pad(dpad)
        else:
            gamepad.left_joystick(x_value=normalize_axis(axes[0], ds4=False),
                                  y_value=normalize_axis(axes[1], invert=True, ds4=False))
            gamepad.right_joystick(x_value=normalize_axis(axes[3], ds4=False),
                                   y_value=normalize_axis(axes[4], invert=True, ds4=False))
            gamepad.left_trigger(int((axes[2] + 1) * 127.5))
            gamepad.right_trigger(int((axes[5] + 1) * 127.5))
            if hats and hats[0]:
                dpad_x, dpad_y = hats[0]
                for d in [
                    vg.XUSB_BUTTON.XUSB_GAMEPAD_DPAD_UP,
                    vg.XUSB_BUTTON.XUSB_GAMEPAD_DPAD_DOWN,
                    vg.XUSB_BUTTON.XUSB_GAMEPAD_DPAD_LEFT,
                    vg.XUSB_BUTTON.XUSB_GAMEPAD_DPAD_RIGHT
                ]:
                    gamepad.release_button(d)
                if dpad_y == 1:
                    gamepad.press_button(vg.XUSB_BUTTON.XUSB_GAMEPAD_DPAD_UP)
                elif dpad_y == -1:
                    gamepad.press_button(vg.XUSB_BUTTON.XUSB_GAMEPAD_DPAD_DOWN)
                if dpad_x == -1:
                    gamepad.press_button(vg.XUSB_BUTTON.XUSB_GAMEPAD_DPAD_LEFT)
                elif dpad_x == 1:
                    gamepad.press_button(vg.XUSB_BUTTON.XUSB_GAMEPAD_DPAD_RIGHT)

        gamepad.update()
        time.sleep(0.005)

# 🧠 Main listener process
def main():
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.bind((UDP_IP, UDP_PORT))
    print(f"🎧 Listening on UDP {UDP_PORT} for input packets...")

    controller_queues = {}
    controller_processes = {}

    try:
        while True:
            data, _ = sock.recvfrom(1024)
            try:
                decoded = data.decode(errors='ignore')
                if "|raw:" in decoded:
                    # Raw packet detected — skip parsing for now, or handle later
                    name, raw_data = decoded.split("|raw:")
                    raw_data = raw_data.strip()
                    # You can handle it here if needed
                    # For now, just print cleanly and skip processing
                    print(f"[{name}] 🎮 Received raw packet ({len(raw_data)} bytes)")
                    continue
                else:
                    parts = decoded.split("|")
                    if len(parts) < 5:
                        print(f"⚠️ Malformed packet (not enough parts)")
                        print(f"📦 Packet: {decoded.strip()}")
                        continue

                    name, buttons_str, axes_str, hats_str, battery_str = parts
                    buttons = ast.literal_eval(buttons_str)
                    axes = ast.literal_eval(axes_str)
                    hats = ast.literal_eval(hats_str)
                    battery = battery_str.replace("Battery:", "").strip()

            except Exception as e:
                print(f"❌ Failed to parse packet: {e}")
                continue

            if name not in controller_queues:
                print(f"[{name}] 🔋 Battery: {battery}%")
                q = multiprocessing.Queue()
                p = multiprocessing.Process(target=controller_process, args=(name, q, USE_DS4))
                p.start()
                controller_queues[name] = q
                controller_processes[name] = p

            controller_queues[name].put((buttons, axes, hats))

    except KeyboardInterrupt:
        print("🛑 Shutting down...")
    finally:
        for p in controller_processes.values():
            p.terminate()
        sock.close()

if __name__ == "__main__":
    multiprocessing.set_start_method("spawn")
    main()