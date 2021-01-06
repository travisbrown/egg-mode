// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

//! Types and methods for looking up locations.
//!
//! Location search for Twitter works in one of two ways. The most direct method is to take a
//! latitude/longitude coordinate (say, from a devide's GPS system or by geolocating from wi-fi
//! networks, or simply from a known coordinate) and call `reverse_geocode`. Twitter says
//! `reverse_geocode` provides more of a "raw data access", and it can be considered to merely show
//! what locations are in that point or area.
//!
//! On the other hand, if you're intending to let a user select from a list of locations, you can
//! use the `search_*` methods instead. These have much of the same available parameters, but will
//! "potentially re-order \[results\] with regards to the user who is authenticated." In addition,
//! the results may potentially pull in "nearby" results to allow for a more broad selection or to
//! account for inaccurate location reporting.
//!
//! Since there are several optional parameters to both query methods, each one is assembled as a
//! builder. You can create the builder with the `reverse_geocode`, `search_point`, `search_query`,
//! or `search_ip` functions. From there, add any additional parameters by chaining method calls
//! onto the builder. When you're ready to peform the search call, hand your tokens to `call`, and
//! the list of results will be returned.
//!
//! Along with the list of place results, Twitter also returns the full search URL. egg-mode
//! returns this URL as part of the result struct, allowing you to perform the same search using
//! the `reverse_geocode_url` or `search_url` functions.

use std::collections::HashMap;
use std::fmt;

use serde::de::Error;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json;

use crate::common::*;
use crate::{auth, error, links};

mod fun;

pub use self::fun::*;

// https://developer.twitter.com/en/docs/tweets/data-dictionary/overview/geo-objects#place
///Represents a named location.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Place {
    ///Alphanumeric ID of the location.
    pub id: String,
    ///Map of miscellaneous information about this place. See [Twitter's documentation][attrib] for
    ///details and common attribute keys.
    ///
    ///[attrib]: https://developer.twitter.com/en/docs/tweets/data-dictionary/overview/geo-objects#place
    pub attributes: HashMap<String, String>,
    ///A bounding box of latitude/longitude coordinates that encloses this place.
    #[serde(with = "serde_bounding_box")]
    pub bounding_box: Vec<(f64, f64)>,
    ///Name of the country containing this place.
    pub country: String,
    ///Shortened country code representing the country containing this place.
    pub country_code: String,
    ///Full human-readable name of this place.
    pub full_name: String,
    ///Short human-readable name of this place.
    pub name: String,
    ///The type of location represented by this place.
    pub place_type: PlaceType,
    ///If present, the country or administrative region that contains this place.
    pub contained_within: Option<Vec<Place>>,
}

///Represents the type of region represented by a given place.
#[derive(Debug, Copy, Clone, Serialize, Deserialize)]
pub enum PlaceType {
    ///A coordinate with no area.
    #[serde(rename = "poi")]
    PointOfInterest,
    ///A region within a city.
    #[serde(rename = "neighborhood")]
    Neighborhood,
    ///An entire city.
    #[serde(rename = "city")]
    City,
    ///An administrative area, e.g. state or province.
    #[serde(rename = "admin")]
    Admin,
    ///An entire country.
    #[serde(rename = "country")]
    Country,
}

///Represents the accuracy of a GPS measurement, when being given to a location search.
#[derive(Debug, Copy, Clone)]
pub enum Accuracy {
    ///Location accurate to the given number of meters.
    Meters(f64),
    ///Location accurate to the given number of feet.
    Feet(f64),
}

///Represents the result of a location search, either via `reverse_geocode` or `search`.
pub struct SearchResult {
    ///The full URL used to pull the result list. This can be fed to the `_url` version of your
    ///original call to avoid having to fill out the argument list again.
    pub url: String,
    ///The list of results from the search.
    pub results: Vec<Place>,
}

impl<'de> Deserialize<'de> for SearchResult {
    fn deserialize<D>(deser: D) -> Result<SearchResult, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw: serde_json::Value = serde_json::Value::deserialize(deser)?;
        let url = raw
            .get("query")
            .and_then(|obj| obj.get("url"))
            .ok_or_else(|| D::Error::custom("Malformed search result"))?
            .to_string();
        let results = raw
            .get("result")
            .and_then(|obj| obj.get("places"))
            .and_then(|arr| <Vec<Place>>::deserialize(arr).ok())
            .ok_or_else(|| D::Error::custom("Malformed search result"))?;
        Ok(SearchResult { url, results })
    }
}

///Represents a `reverse_geocode` query before it is sent.
///
///The available methods on this builder struct allow you to specify optional parameters to the
///search operation. Where applicable, each method lists its default value and acceptable ranges.
///
///To complete your search setup and send the query to Twitter, hand your tokens to `call`. The
///list of results from Twitter will be returned, as well as a URL to perform the same search via
///`reverse_geocode_url`.
pub struct GeocodeBuilder {
    coordinate: (f64, f64),
    accuracy: Option<Accuracy>,
    granularity: Option<PlaceType>,
    max_results: Option<u32>,
}

impl GeocodeBuilder {
    ///Begins building a reverse-geocode query with the given coordinate.
    fn new(latitude: f64, longitude: f64) -> Self {
        GeocodeBuilder {
            coordinate: (latitude, longitude),
            accuracy: None,
            granularity: None,
            max_results: None,
        }
    }

    ///Expands the area to search to the given radius. By default, this is zero.
    ///
    ///From Twitter: "If coming from a device, in practice, this value is whatever accuracy the
    ///device has measuring its location (whether it be coming from a GPS, WiFi triangulation,
    ///etc.)."
    pub fn accuracy(self, accuracy: Accuracy) -> Self {
        GeocodeBuilder {
            accuracy: Some(accuracy),
            ..self
        }
    }

    ///Sets the minimal specificity of what kind of results to return. For example, passing `City`
    ///to this will make the eventual result exclude neighborhoods and points.
    pub fn granularity(self, granularity: PlaceType) -> Self {
        GeocodeBuilder {
            granularity: Some(granularity),
            ..self
        }
    }

    ///Restricts the maximum number of results returned in this search. This is not a guarantee
    ///that the search will return this many results, but instead provides a hint as to how many
    ///"nearby" results to return.
    ///
    ///This value has a default value of 20, which is also its maximum. If zero or a number greater
    ///than 20 is passed here, it will be defaulted to 20 before sending to Twitter.
    ///
    ///From Twitter: "Ideally, only pass in the number of places you intend to display to the user
    ///here."
    pub fn max_results(self, max_results: u32) -> Self {
        GeocodeBuilder {
            max_results: Some(max_results),
            ..self
        }
    }

    ///Finalize the search parameters and return the results collection.
    pub async fn call(&self, token: &auth::Token) -> Result<Response<SearchResult>, error::Error> {
        let params = ParamList::new()
            .add_param("lat", self.coordinate.0.to_string())
            .add_param("long", self.coordinate.1.to_string())
            .add_opt_param("accuracy", self.accuracy.map_string())
            .add_opt_param("granularity", self.granularity.map_string())
            .add_opt_param(
                "max_results",
                self.max_results.map(|count| {
                    let count = if count == 0 || count > 20 { 20 } else { count };
                    count.to_string()
                }),
            );

        let req = get(links::place::REVERSE_GEOCODE, token, Some(&params));
        request_with_json_response(req).await
    }
}

enum PlaceQuery {
    LatLon(f64, f64),
    Query(CowStr),
    IPAddress(CowStr),
}

///Represents a location search query before it is sent.
///
///The available methods on this builder struct allow you to specify optional parameters to the
///search operation. Where applicable, each method lists its default value and acceptable ranges.
///
///To complete your search setup and send the query to Twitter, hand your tokens to `call`. The
///list of results from Twitter will be returned, as well as a URL to perform the same search via
///`search_url`.
pub struct SearchBuilder {
    query: PlaceQuery,
    accuracy: Option<Accuracy>,
    granularity: Option<PlaceType>,
    max_results: Option<u32>,
    contained_within: Option<String>,
    attributes: Option<HashMap<String, String>>,
}

impl SearchBuilder {
    ///Begins building a location search with the given query.
    fn new(query: PlaceQuery) -> Self {
        SearchBuilder {
            query: query,
            accuracy: None,
            granularity: None,
            max_results: None,
            contained_within: None,
            attributes: None,
        }
    }

    ///Expands the area to search to the given radius. By default, this is zero.
    ///
    ///From Twitter: "If coming from a device, in practice, this value is whatever accuracy the
    ///device has measuring its location (whether it be coming from a GPS, WiFi triangulation,
    ///etc.)."
    pub fn accuracy(self, accuracy: Accuracy) -> Self {
        SearchBuilder {
            accuracy: Some(accuracy),
            ..self
        }
    }

    ///Sets the minimal specificity of what kind of results to return. For example, passing `City`
    ///to this will make the eventual result exclude neighborhoods and points.
    pub fn granularity(self, granularity: PlaceType) -> Self {
        SearchBuilder {
            granularity: Some(granularity),
            ..self
        }
    }

    ///Restricts the maximum number of results returned in this search. This is not a guarantee
    ///that the search will return this many results, but instead provides a hint as to how many
    ///"nearby" results to return.
    ///
    ///From experimentation, this value has a default of 20 and a maximum of 100. If fewer
    ///locations match the search parameters, fewer places will be returned.
    ///
    ///From Twitter: "Ideally, only pass in the number of places you intend to display to the user
    ///here."
    pub fn max_results(self, max_results: u32) -> Self {
        SearchBuilder {
            max_results: Some(max_results),
            ..self
        }
    }

    ///Restricts results to those contained within the given Place ID.
    pub fn contained_within(self, contained_id: String) -> Self {
        SearchBuilder {
            contained_within: Some(contained_id),
            ..self
        }
    }

    ///Restricts results to those with the given attribute. A list of common attributes are
    ///available in [Twitter's documentation for Places][attrs]. Custom attributes are supported in
    ///this search, if you know them. This function may be called multiple times with different
    ///`attribute_key` values to combine attribute search parameters.
    ///
    ///[attrs]: https://developer.twitter.com/en/docs/tweets/data-dictionary/overview/geo-objects#place
    ///
    ///For example, `.attribute("street_address", "123 Main St")` searches for places with the
    ///given street address.
    pub fn attribute(self, attribute_key: String, attribute_value: String) -> Self {
        let mut attrs = self.attributes.unwrap_or_default();
        attrs.insert(attribute_key, attribute_value);

        SearchBuilder {
            attributes: Some(attrs),
            ..self
        }
    }

    ///Finalize the search parameters and return the results collection.
    pub async fn call(&self, token: &auth::Token) -> Result<Response<SearchResult>, error::Error> {
        let mut params = match &self.query {
            PlaceQuery::LatLon(lat, long) => ParamList::new()
                .add_param("lat", lat.to_string())
                .add_param("long", long.to_string()),
            PlaceQuery::Query(text) => ParamList::new().add_param("query", text.to_string()),
            PlaceQuery::IPAddress(text) => ParamList::new().add_param("ip", text.to_string()),
        }
        .add_opt_param("accuracy", self.accuracy.map_string())
        .add_opt_param("granularity", self.granularity.map_string())
        .add_opt_param("max_results", self.max_results.map_string())
        .add_opt_param("contained_within", self.contained_within.map_string());

        if let Some(ref attrs) = self.attributes {
            for (k, v) in attrs {
                params.add_param_ref(format!("attribute:{}", k), v.clone());
            }
        }

        let req = get(links::place::SEARCH, token, Some(&params));
        request_with_json_response(req).await
    }
}

///Display impl to make `to_string()` format the enum for sending to Twitter. This is *mostly* just
///a lowercase version of the variants, but `Point` is rendered as `"poi"` instead.
impl fmt::Display for PlaceType {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let quoted = serde_json::to_string(self).unwrap();
        let inner = &quoted[1..quoted.len() - 1]; // ignore the quote marks
        write!(f, "{}", inner)
    }
}

///Display impl to make `to_string()` format the enum for sending to Twitter. This turns `Meters`
///into the contained number by itself, and `Feet` into the number suffixed by `"ft"`.
impl fmt::Display for Accuracy {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match *self {
            Accuracy::Meters(dist) => write!(f, "{}", dist),
            Accuracy::Feet(dist) => write!(f, "{}ft", dist),
        }
    }
}

mod serde_bounding_box {
    use serde::{Serialize, Deserialize, Serializer, Deserializer};
    use serde::de::Error;

    pub fn deserialize<'de, D>(ser: D) -> Result<Vec<(f64, f64)>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = serde_json::Value::deserialize(ser)?;
        if s.is_null() {
            Ok(vec![])
        } else {
            s.get("coordinates")
                .and_then(|arr| arr.get(0).cloned())
                .ok_or_else(|| D::Error::custom("Malformed 'bounding_box' attribute"))
                .and_then(|inner_arr| {
                    serde_json::from_value::<Vec<(f64, f64)>>(inner_arr)
                        .map_err(|e| D::Error::custom(e))
                })
        }
    }

    pub fn serialize<S>(src: &Vec<(f64, f64)>, ser: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        #[derive(Serialize)]
        struct SerBox {
            coordinates: Vec<(f64, f64)>,
            #[serde(rename = "type")]
            box_type: BoxType,
        }

        #[derive(Serialize)]
        enum BoxType {
            Polygon,
            Point,
        }

        impl From<&Vec<(f64, f64)>> for SerBox {
            fn from(src: &Vec<(f64, f64)>) -> SerBox {
                let box_type = if src.len() == 1 {
                    BoxType::Point
                } else {
                    BoxType::Polygon
                };

                SerBox {
                    coordinates: src.clone(),
                    box_type,
                }
            }
        }

        let out: Option<SerBox> = if src.is_empty() {
            None
        } else {
            Some(src.into())
        };
        out.serialize(ser)
    }
}
