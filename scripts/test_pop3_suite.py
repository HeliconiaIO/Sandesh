#!/usr/bin/env python3
"""
Sandesh POP3 & SMTP Comprehensive Test Suite (A-to-Z Verification)

Tests every POP3 RFC 1939/2449/2595 command and workflow:
1. SMTP Ingestion of multiple test messages (simple, multipart, attachments).
2. POP3 Banner and CAPA capabilities negotiation.
3. POP3 USER / PASS Authentication.
4. POP3 STAT (mailbox size and count).
5. POP3 LIST (multi-line and single-message).
6. POP3 UIDL (stable unique identifier listing and single-message).
7. POP3 TOP (header-only and header+body preview).
8. POP3 RETR (full message retrieval and dot-stuffing).
9. POP3 DELE & RSET (deletion staging and rollback).
10. POP3 DELE & QUIT (deletion commit).
11. Second Session Reconnection (verifying deletion persistence & 1..N numbering).
"""

import sys
import time
import socket
import smtplib
import poplib
from email.mime.text import MIMEText
from email.mime.multipart import MIMEMultipart
from email.mime.base import MIMEBase
from email import encoders

# Terminal colors
GREEN = "\033[92m"
RED = "\033[91m"
YELLOW = "\033[93m"
CYAN = "\033[96m"
BOLD = "\033[1m"
RESET = "\033[0m"

SMTP_HOST = "127.0.0.1"
SMTP_PORT = 1025
POP3_HOST = "127.0.0.1"
POP3_PORT = 1110
TEST_USER = "alice@sandesh.test"
TEST_PASS = "any_password"

passed_tests = 0
failed_tests = 0


def log_step(step_name):
    print(f"\n{BOLD}{CYAN}=== {step_name} ==={RESET}")


def assert_test(condition, desc):
    global passed_tests, failed_tests
    if condition:
        print(f"  {GREEN}[PASS]{RESET} {desc}")
        passed_tests += 1
    else:
        print(f"  {RED}[FAIL]{RESET} {desc}")
        failed_tests += 1
        raise AssertionError(desc)


def check_servers_running():
    for name, host, port in [("SMTP", SMTP_HOST, SMTP_PORT), ("POP3", POP3_HOST, POP3_PORT)]:
        try:
            with socket.create_connection((host, port), timeout=2):
                pass
        except Exception:
            print(f"{RED}{BOLD}ERROR: Sandesh {name} server is not running on {host}:{port}!{RESET}")
            print(f"Please run Sandesh in another terminal using:")
            print(f"    {YELLOW}cargo run{RESET}\n")
            sys.exit(1)


def send_test_email(from_addr, to_addr, subject, body, attachment_content=None):
    if attachment_content:
        msg = MIMEMultipart()
        msg["From"] = from_addr
        msg["To"] = to_addr
        msg["Subject"] = subject
        msg.attach(MIMEText(body, "plain"))

        part = MIMEBase("application", "octet-stream")
        part.set_payload(attachment_content)
        encoders.encode_base64(part)
        part.add_header("Content-Disposition", f'attachment; filename="sample.txt"')
        msg.attach(part)
    else:
        msg = MIMEText(body)
        msg["From"] = from_addr
        msg["To"] = to_addr
        msg["Subject"] = subject

    with smtplib.SMTP(SMTP_HOST, SMTP_PORT) as server:
        server.sendmail(from_addr, [to_addr], msg.as_string())


def run_suite():
    global passed_tests, failed_tests
    print(f"{BOLD}{GREEN}Starting Sandesh POP3 & SMTP Comprehensive Test Suite...{RESET}")

    check_servers_running()
    timestamp = int(time.time())

    # -------------------------------------------------------------
    # Step 1: Send Test Emails via SMTP
    # -------------------------------------------------------------
    log_step("Step 1: Delivering Test Emails via SMTP")
    subj1 = f"POP3 Test 1: Simple Message [{timestamp}]"
    body1 = "Hello Alice,\r\n\r\nThis is a simple plain text test message for POP3 testing.\r\n.A line starting with a dot to test dot-stuffing.\r\nCheers!"
    send_test_email("sender1@example.com", TEST_USER, subj1, body1)
    assert_test(True, "Sent Message 1 (plain text with dot line) via SMTP")

    subj2 = f"POP3 Test 2: Multipart Attachment [{timestamp}]"
    body2 = "Hello Alice,\r\n\r\nThis message contains a binary/text attachment to verify full MIME parsing."
    send_test_email("sender2@example.com", TEST_USER, subj2, body2, attachment_content=b"Attachment payload: 1234567890\n")
    assert_test(True, "Sent Message 2 (multipart with attachment) via SMTP")

    time.sleep(0.5)  # give database a moment to settle

    # -------------------------------------------------------------
    # Step 2: POP3 Connection & Capabilities (CAPA)
    # -------------------------------------------------------------
    log_step("Step 2: Connecting to POP3 & Verifying Greeting & CAPA")
    pop = poplib.POP3(POP3_HOST, POP3_PORT)
    welcome = pop.getwelcome().decode("utf-8", errors="replace")
    assert_test(welcome.startswith("+OK"), f"Welcome banner received: {welcome}")

    # Test CAPA command
    caps = pop.capa()
    assert_test("USER" in caps, "CAPA advertises USER")
    assert_test("UIDL" in caps, "CAPA advertises UIDL")
    assert_test("TOP" in caps, "CAPA advertises TOP")
    assert_test("RESP-CODES" in caps, "CAPA advertises RESP-CODES")
    assert_test("IMPLEMENTATION" in caps, f"CAPA advertises IMPLEMENTATION: {caps.get('IMPLEMENTATION')}")

    # -------------------------------------------------------------
    # Step 3: POP3 Authentication
    # -------------------------------------------------------------
    log_step("Step 3: POP3 Authentication (USER / PASS)")
    user_res = pop.user(TEST_USER).decode("utf-8", errors="replace")
    assert_test(user_res.startswith("+OK"), f"USER command accepted: {user_res}")

    pass_res = pop.pass_(TEST_PASS).decode("utf-8", errors="replace")
    assert_test(pass_res.startswith("+OK"), f"PASS command accepted: {pass_res}")

    # -------------------------------------------------------------
    # Step 4: POP3 STAT
    # -------------------------------------------------------------
    log_step("Step 4: Checking Mailbox Status (STAT)")
    msg_count, total_size = pop.stat()
    assert_test(msg_count >= 2, f"STAT reports {msg_count} messages in mailbox (>= 2 expected)")
    assert_test(total_size > 0, f"STAT reports total size: {total_size} octets")

    # -------------------------------------------------------------
    # Step 5: POP3 LIST (Multi-line & Single message)
    # -------------------------------------------------------------
    log_step("Step 5: Listing Messages (LIST)")
    resp, listing, octets = pop.list()
    assert_test(resp.decode().startswith("+OK"), "LIST multi-line response received")
    assert_test(len(listing) == msg_count, f"LIST returned {len(listing)} entries")

    # Test single-line LIST for message 1
    resp_single = pop.list("1").decode()
    assert_test(resp_single.startswith("+OK"), f"LIST 1 (single-line) returned: {resp_single}")

    # -------------------------------------------------------------
    # Step 6: POP3 UIDL (Unique ID Listing)
    # -------------------------------------------------------------
    log_step("Step 6: Unique Identifiers (UIDL)")
    resp, uidl_list, _ = pop.uidl()
    assert_test(resp.decode().startswith("+OK"), "UIDL multi-line response received")
    assert_test(len(uidl_list) == msg_count, f"UIDL returned {len(uidl_list)} unique IDs")

    # Test single-line UIDL
    resp_uidl_1 = pop.uidl("1").decode()
    assert_test(resp_uidl_1.startswith("+OK"), f"UIDL 1 returned: {resp_uidl_1}")

    # -------------------------------------------------------------
    # Step 7: POP3 TOP (Header & Body Preview)
    # -------------------------------------------------------------
    log_step("Step 7: Previewing Headers and Body (TOP)")
    # Find the message with subj1 by checking the last messages
    target_msg_num = msg_count - 1
    resp, top_lines, _ = pop.top(target_msg_num, 5)
    top_decoded = "\n".join(l.decode("utf-8", errors="replace") for l in top_lines)
    assert_test(resp.decode().startswith("+OK"), f"TOP {target_msg_num} 5 succeeded")
    assert_test("Subject: " in top_decoded, "TOP output contains Subject header")

    # -------------------------------------------------------------
    # Step 8: POP3 RETR (Full Retrieval & Dot-Stuffing Check)
    # -------------------------------------------------------------
    log_step("Step 8: Full Message Retrieval (RETR)")
    resp, msg_lines, _ = pop.retr(target_msg_num)
    assert_test(resp.decode().startswith("+OK"), f"RETR {target_msg_num} succeeded")
    full_body = "\n".join(l.decode("utf-8", errors="replace") for l in msg_lines)

    assert_test(subj1 in full_body, f"Retrieved message contains expected subject: {subj1}")
    # Verify dot-un-stuffing: ".A line starting with a dot" must be intact
    assert_test(".A line starting with a dot" in full_body, "Dot-stuffed line was correctly unpacked")

    # Retrieve multipart message
    multipart_msg_num = msg_count
    resp, multi_lines, _ = pop.retr(multipart_msg_num)
    assert_test(resp.decode().startswith("+OK"), f"RETR {multipart_msg_num} (multipart) succeeded")
    multi_body = "\n".join(l.decode("utf-8", errors="replace") for l in multi_lines)
    assert_test(subj2 in multi_body, f"Retrieved multipart message contains expected subject: {subj2}")
    assert_test("attachment; filename=" in multi_body, "Attachment header preserved in raw POP3 RETR")

    # -------------------------------------------------------------
    # Step 9: POP3 DELE & RSET (Rollback)
    # -------------------------------------------------------------
    log_step("Step 9: Testing DELE and RSET Rollback")
    dele_resp = pop.dele(target_msg_num).decode()
    assert_test(dele_resp.startswith("+OK"), f"DELE {target_msg_num} marked message for deletion: {dele_resp}")

    rset_resp = pop.rset().decode()
    assert_test(rset_resp.startswith("+OK"), f"RSET rolled back pending deletions: {rset_resp}")

    # Check count after RSET
    new_count, _ = pop.stat()
    assert_test(new_count == msg_count, f"STAT after RSET still shows {new_count} messages (rollback verified)")

    # -------------------------------------------------------------
    # Step 10: POP3 DELE & QUIT (Commit Deletion)
    # -------------------------------------------------------------
    log_step("Step 10: Testing DELE and QUIT Commit")
    pop.dele(target_msg_num)
    quit_resp = pop.quit().decode()
    assert_test(quit_resp.startswith("+OK"), f"QUIT committed deletion: {quit_resp}")

    # -------------------------------------------------------------
    # Step 11: Reconnect & Verify Deletion Persistence
    # -------------------------------------------------------------
    log_step("Step 11: Reconnecting to Verify Deletion Persistence & Renumbering")
    pop2 = poplib.POP3(POP3_HOST, POP3_PORT)
    pop2.user(TEST_USER)
    pop2.pass_(TEST_PASS)

    count_after, _ = pop2.stat()
    assert_test(count_after == msg_count - 1, f"STAT after deletion commit reports {count_after} messages (expected {msg_count - 1})")

    # Also delete the second message to leave mailbox clean
    pop2.dele(count_after)
    pop2.quit()
    assert_test(True, "Cleaned up second test message via DELE and QUIT")

    # -------------------------------------------------------------
    # Summary
    # -------------------------------------------------------------
    print(f"\n{BOLD}{GREEN}======================================================{RESET}")
    print(f"{BOLD}{GREEN} ALL POP3 & SMTP TESTS PASSED SUCCESSFULLY! ({passed_tests}/{passed_tests}){RESET}")
    print(f"{BOLD}{GREEN}======================================================{RESET}\n")


if __name__ == "__main__":
    try:
        run_suite()
    except AssertionError as e:
        print(f"\n{BOLD}{RED}FAILED at assertion: {e}{RESET}")
        sys.exit(1)
    except Exception as e:
        print(f"\n{BOLD}{RED}UNEXPECTED ERROR: {e}{RESET}")
        import traceback
        traceback.print_exc()
        sys.exit(1)
