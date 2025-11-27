# Example of a valid terragrunt project using generate blocks
# This mimics the organization.hcl pattern from ***REMOVED***/infra

terraform {}

generate "provider" {
  path      = "provider.tf"
  if_exists = "overwrite"
  contents  = <<EOF
provider "aws" {
  region = "us-east-1"
}
EOF
}

generate "main" {
  path      = "main.tf"
  if_exists = "overwrite"
  contents  = <<EOF
resource "aws_organizations_organization" "main" {
  feature_set = "ALL"
}
EOF
}
