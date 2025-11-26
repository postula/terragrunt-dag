locals {
  # These don't reference files - should not extract anything
  region      = "us-east-1"
  environment = "prod"
  tags = {
    Environment = local.environment
  }
}
