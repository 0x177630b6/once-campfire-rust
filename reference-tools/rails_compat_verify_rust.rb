# Verifies, with the reference app, the values crates/rails_compat generated
# (target/rails_compat_rust_output.json, written by `cargo test -p rails_compat`).
# Cookies and CSRF tokens also go through full requests against the app, the "old browser tab"
# direction: a session Rust started is used to sign in on Rails, and a Rust-signed session_token
# cookie authenticates on Rails.
#
#   reference-tools/run.sh reference-tools/rails_compat_verify_rust.rb [path/to/rust_output.json]
require_relative "support"

class RailsCompatVerifyRust
  include ReferenceTools

  def initialize(path)
    @output = JSON.parse(File.read(path))
    @failures = []
    @checks = 0
  end

  def run
    reset_database!
    travel_to(Time.iso8601(@output.fetch("now")))

    verify_signed_cookies
    verify_encrypted_cookies
    verify_session_requests
    verify_csrf_tokens
    verify_signed_ids
    verify_sgids
    verify_turbo_stream_names
    verify_passwords

    travel_back
    report
  end

  private
    def check(label, actual, expected)
      @checks += 1
      @failures << "#{label}: expected #{expected.inspect}, got #{actual.inspect}" unless actual == expected
    end

    def verify_signed_cookies
      @output["signed_cookies"].each do |cookie|
        check "signed cookie #{cookie["value"].inspect}", read_cookie(:signed, cookie["name"], cookie["raw"]), cookie["value"]
        check "signed cookie #{cookie["value"].inspect} under another name", read_cookie(:signed, "other_name", cookie["raw"]), nil
      end
    end

    def verify_encrypted_cookies
      @output["encrypted_cookies"].each do |cookie|
        check "encrypted cookie #{cookie["value"].inspect}", read_cookie(:encrypted, cookie["name"], cookie["raw"]), cookie["value"]
        check "encrypted cookie #{cookie["value"].inspect} under another name", read_cookie(:encrypted, "other_name", cookie["raw"]), nil
      end
    end

    def verify_session_requests
      session = @output["session"]
      cookies = { "_campfire_session" => session["cookie"] }
      credentials = { email_address: "david@example.com", password: "secret123456" }

      status, headers, _ = perform(:post, "/session", cookies: cookies, params: credentials.merge(authenticity_token: session["form_token"]))
      check "sign in with a Rust session and per-form token", status, 302
      set = set_cookies(headers)
      check "Rails keeps the Rust session id", read_cookie(:encrypted, "_campfire_session", set.dig("_campfire_session", "raw"))&.fetch("session_id"), session["session_id"]

      status, _, _ = perform(:post, "/session", cookies: cookies, params: credentials, headers: { "HTTP_X_CSRF_TOKEN" => session["meta_token"] })
      check "sign in with a Rust session and X-CSRF-Token", status, 302

      status, _, _ = perform(:post, "/session", cookies: cookies, params: credentials.merge(authenticity_token: session["meta_token"].reverse))
      check "sign in with a bad token", status, 422

      Session.create!(user: @david, token: session["session_token"], user_agent: USER_AGENT, ip_address: "127.0.0.1")
      status, headers, _ = perform(:get, "/", cookies: { "session_token" => session["session_token_cookie"] })
      check "Rust-signed session_token authenticates (not a redirect to sign in)", [ status, headers["location"].to_s.include?("/session/new") ], [ status, false ]
      check "Rust-signed session_token authenticates (status)", status < 400, true

      status, headers, _ = perform(:get, "/", cookies: { "session_token" => session["session_token_cookie"].reverse })
      check "tampered session_token redirects to sign in", headers["location"].to_s.include?("/session/new"), true
    end

    def verify_csrf_tokens
      @output["csrf"]["tokens"].each do |token|
        controller = csrf_controller(@output["csrf"]["session_token"], path: token["path"], method: token["method"])
        check "csrf token for #{token["method"]} #{token["path"]}", controller.send(:valid_authenticity_token?, controller.session, token["token"]), token["expected"]
      end
    end

    def verify_signed_ids
      @output["signed_ids"].each do |signed|
        purpose = User.combine_signed_id_purposes(signed["purpose"])
        check "signed id #{signed["id"]} #{signed["purpose"]}", User.signed_id_verifier.verified(signed["signed_id"], purpose: purpose), signed["id"]
        check "signed id #{signed["id"]} with another purpose", User.signed_id_verifier.verified(signed["signed_id"], purpose: "user/other"), nil
      end
      avatar = @output["signed_ids"].find { |s| s["purpose"] == "avatar" && s["id"] == @david.id }
      check "find_signed! for avatar", User.find_signed!(avatar["signed_id"], purpose: :avatar), @david
    end

    def verify_sgids
      @output["sgids"].each do |sgid|
        check "sgid #{sgid["expected"]}", SignedGlobalID.parse(sgid["sgid"], for: sgid["purpose"])&.uri&.to_s, sgid["expected"]
      end
      attachable = @output["sgids"].first["sgid"]
      check "ActionText::Attachable.from_attachable_sgid", ActionText::Attachable.from_attachable_sgid(attachable), @david
      check "Rust attachable_sgid equals Rails'", attachable, @david.attachable_sgid
    end

    def verify_turbo_stream_names
      @output["turbo_stream_names"].each do |stream|
        check "turbo stream #{stream["expected"]}", Turbo::StreamsChannel.verified_stream_name(stream["signed"]), stream["expected"]
      end
    end

    def verify_passwords
      @output["passwords"].each do |password|
        check "bcrypt #{password["password"][0, 20]}", BCrypt::Password.new(password["digest"]).is_password?(password["password"]), true
        check "bcrypt wrong password", BCrypt::Password.new(password["digest"]).is_password?("wrong"), false
      end
      user = User.new(password_digest: @output["passwords"].first["digest"])
      check "has_secure_password authenticate", user.authenticate(@output["passwords"].first["password"]), user
    end

    def report
      puts "#{@checks} checks, #{@failures.size} failures"
      @failures.each { |failure| puts "  FAIL #{failure}" }
      # Exiting inside `rails runner`'s executor trips the error reporter, so exit afterwards.
      status = @failures.empty?
      at_exit { exit(status) }
    end
end

default = File.expand_path("../target/rails_compat_rust_output.json", __dir__)
default = "/work/target/rails_compat_rust_output.json" unless File.exist?(default)
RailsCompatVerifyRust.new(ARGV.first || default).run
