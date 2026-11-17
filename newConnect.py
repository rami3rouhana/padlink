import socket
import json
import multiprocessing
import vgamepad as vg
import signal
import sys
import time
import ast

# === CONFIG ===
UDP_PORT = 9999
ACK_PORT = 8888
USE_DS4 = True
HEARTBEAT_TIMEOUT = 10
RETRY_INTERVAL = 5

# === GLOBAL STATE ===
controller_queues = {}
controller_processes = {}
last_pi_addr = None
last_heartbeat = 0
last_retry = 0

# === Signal Handling ===
def shutdown(signum, frame):
    print("\n🛑 Shutting down receiver...")
    for mac, process in controller_processes.items():
        print(f"🛑 Terminating emulator for {mac}")
        process.terminate()
    sys.exit(0)

signal.signal(signal.SIGINT, shutdown)
signal.signal(signal.SIGTERM, shutdown)

# === Emulation Logic ===
def normalize_axis(val, invert=False, ds4=True):
    if abs(val) < 0.1:
        val = 0.0
    val = max(-1.0, min(1.0, val))
    return int(((-val if invert else val) + 1.0) * 127.5) if ds4 else int((-val if invert else val) * 32767)

def controller_process(mac, queue, use_ds4):
    print(f"[{mac}] 🎮 Emulation started")
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
            9: vg.DS4_BUTTONS.DS4_BUTTON_OPTIONS
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
            6: vg.XUSB_BUTTON.XUSB_GAMEPAD_LEFT_THUMB,
            7: vg.XUSB_BUTTON.XUSB_GAMEPAD_RIGHT_THUMB,
            8: vg.XUSB_BUTTON.XUSB_GAMEPAD_BACK,
            9: vg.XUSB_BUTTON.XUSB_GAMEPAD_START,
            10: vg.XUSB_BUTTON.XUSB_GAMEPAD_GUIDE
        }

    while True:
        try:
            buttons, axes, hats = queue.get(timeout=2)
        except:
            continue

        for idx, state in enumerate(buttons):
            if idx in BUTTON_MAP:
                button = BUTTON_MAP[idx]
                if state:
                    gamepad.press_button(button)
                else:
                    gamepad.release_button(button)

        if use_ds4:
            gamepad.left_joystick(x_value=normalize_axis(axes[0], ds4=True),
                                  y_value=normalize_axis(axes[1], ds4=True))
            gamepad.right_joystick(x_value=normalize_axis(axes[3], ds4=True),
                                   y_value=normalize_axis(axes[4], ds4=True))
            gamepad.left_trigger(normalize_axis(axes[2], ds4=True))
            gamepad.right_trigger(normalize_axis(axes[5], ds4=True))
            if hats:
                gamepad.directional_pad(DPAD_MAP.get(hats[0], vg.DS4_DPAD_DIRECTIONS.DS4_BUTTON_DPAD_NONE))
        else:
            gamepad.left_joystick(x_value=normalize_axis(axes[0], ds4=False),
                                  y_value=normalize_axis(axes[1], invert=True, ds4=False))
            gamepad.right_joystick(x_value=normalize_axis(axes[3], ds4=False),
                                   y_value=normalize_axis(axes[4], invert=True, ds4=False))
            gamepad.left_trigger(int((axes[2] + 1) * 127.5))
            gamepad.right_trigger(int((axes[5] + 1) * 127.5))
            if hats:
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

# === Main Receiver ===
def main():
    global last_pi_addr, last_heartbeat, last_retry
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    sock.bind(('', UDP_PORT))
    sock.settimeout(1.0)
    print(f"🎧 Listening on UDP port {UDP_PORT}...")

    while True:
        now = time.time()

        # Retry ACK if heartbeat goes quiet
        if last_pi_addr and now - last_heartbeat > HEARTBEAT_TIMEOUT and now - last_retry > RETRY_INTERVAL:
            try:
                ack = json.dumps({"type": "receiver_ack"}).encode()
                sock.sendto(ack, last_pi_addr)
                print(f"⚠️ Retried ACK to {last_pi_addr}")
                last_retry = now
            except Exception as e:
                print(f"❌ Failed to resend ACK: {e}")

        try:
            data, addr = sock.recvfrom(4096)
        except socket.timeout:
            continue

        try:
            decoded = data.decode(errors='ignore').strip()

            if "|raw:" in decoded:
                continue

            if decoded.startswith("{") and '"controllers"' in decoded:
                packet = json.loads(decoded)
                last_pi_addr = (addr[0], ACK_PORT)
                last_heartbeat = now
                last_retry = 0
                sock.sendto(json.dumps({"type": "receiver_ack"}).encode(), last_pi_addr)
                print(f"📡 Heartbeat from {addr[0]} → ✅ ACK sent to {last_pi_addr}")
                continue

            if decoded.startswith("{"):
                json_packet = json.loads(decoded)
                name = json_packet.get("mac", "Unknown")
                battery = json_packet.get("battery", 100)

                buttons = [0] * 12
                axes = [0.0] * 6
                hats = []

                raw_buttons = json_packet.get("buttons", 0)
                for i in range(len(buttons)):
                    buttons[i] = 1 if raw_buttons & (1 << i) else 0

                axes[0] = json_packet["lx"] / 127.5
                axes[1] = json_packet["ly"] / 127.5
                axes[2] = json_packet["l2"] / 255.0
                axes[3] = json_packet["rx"] / 127.5
                axes[4] = json_packet["ry"] / 127.5
                axes[5] = json_packet["r2"] / 255.0
            else:
                parts = decoded.split("|")
                if len(parts) < 5:
                    continue
                name, buttons_str, axes_str, hats_str, battery_str = parts
                buttons = ast.literal_eval(buttons_str)
                axes = ast.literal_eval(axes_str)
                hats = ast.literal_eval(hats_str)
                battery = battery_str.replace("Battery:", "").strip()

        except Exception as e:
            print(f"❌ Failed to parse or process packet: {e}")
            continue

        if name not in controller_queues:
            print(f"[{name}] 🔋 Battery: {battery}% → New controller connected")
            q = multiprocessing.Queue()
            p = multiprocessing.Process(target=controller_process, args=(name, q, USE_DS4))
            p.start()
            controller_queues[name] = q
            controller_processes[name] = p

        controller_queues[name].put((buttons, axes, hats))


if __name__ == "__main__":
    multiprocessing.set_start_method("spawn")
    main()
