c = ApplicationController.new
lc = c.lookup_context
puts ActionView::Digestor.digest(name: "users/avatars/show", format: nil, finder: lc)
lc.formats = [:html]
t = lc.find_all("show", ["users/avatars"]).first
p t&.virtual_path
lc2 = ApplicationController.new.lookup_context
lc2.formats = [:svg]
p lc2.find_all("show", ["users/avatars", "application"]).first&.virtual_path
p ActiveSupport::Digest.hexdigest(File.read("app/views/users/avatars/show.svg.erb") + "-")
p ActiveSupport::Digest.hexdigest(File.read("app/views/users/avatars/show.svg.erb"))
