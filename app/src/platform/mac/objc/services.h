// Our class for handling NSServices messages.
@interface BorkServicesProvider : NSObject
@end

// Functions implemented in Rust.
id bork_services_provider_custom_url_scheme();
void bork_app_open_urls(id app, id urls);
