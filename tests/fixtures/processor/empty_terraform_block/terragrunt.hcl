# Project with empty terraform block (no source attribute)
# This is a valid terragrunt project that uses generate blocks to create terraform code
terraform {}

generate "main" {
  path      = "main.tf"
  if_exists = "overwrite"
  contents  = <<EOF
resource "null_resource" "example" {
  triggers = {
    timestamp = timestamp()
  }
}
EOF
}
