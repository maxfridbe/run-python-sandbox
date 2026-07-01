import sys
import urllib.request
import urllib.error

def try_request(url):
    print(f"[test_network] Accessing {url}...")
    try:
        req = urllib.request.Request(url, headers={'User-Agent': 'Mozilla/5.0'})
        with urllib.request.urlopen(req, timeout=3) as response:
            print(f"[test_network] Connected! Status code: {response.status}")
            return True
    except urllib.error.URLError as e:
        print(f"[test_network] Connection blocked/failed: {e}")
        return False
    except Exception as e:
        print(f"[test_network] Connection error: {e}")
        return False

def main():
    print("=== Running Network Isolation Test ===")
    
    # 1. Access loopback
    localhost_accessible = try_request("http://127.0.0.1:8080")
    
    # 2. Access private IP (RFC 1918)
    private_accessible = try_request("http://192.168.1.1")
    
    # 3. Access cloud metadata (Link-Local)
    metadata_accessible = try_request("http://169.254.169.254/latest/meta-data/")
    
    # 4. Access public internet
    internet_accessible = try_request("https://www.google.com")

    # Assess based on execution expectation. For these tests, we will pass assertions inside the shell test harness.
    # We output results as key-value pairs so the parser in test.sh can check them.
    print(f"RESULT_LOCALHOST={localhost_accessible}")
    print(f"RESULT_PRIVATE={private_accessible}")
    print(f"RESULT_METADATA={metadata_accessible}")
    print(f"RESULT_INTERNET={internet_accessible}")

if __name__ == "__main__":
    main()
